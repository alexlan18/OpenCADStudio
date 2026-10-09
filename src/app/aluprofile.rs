//! Aluminium T-slot profile design — the MayCAD-style workflow on top of the
//! solid modeller: a catalogue of extrusions, profiles placed by length and
//! direction, box frames built in one command, automatic angle brackets at
//! every end-to-face joint, a parts list with part numbers and weights, an
//! optimised cut list for stock bars, and CSV export. Everything is a real
//! 3D solid (kernel B-rep), so the usual tools, sections, STEP/STL/3MF and
//! the AI assistant work on the result.
//!
//! Members are ordinary `3DSOLID` entities tagged with extended data under
//! the `OCS_ALU` application: kind, part number, name, length (mm), start
//! point and direction. The parts list and the joint finder read that tag;
//! an untagged solid is simply not aluminium.
//!
//! | Command | What it does |
//! |---|---|
//! | `ALUPROFILE [profile] [length] [start] [direction]` | Place one profile; missing arguments are asked interactively (profile buttons, length, two points). |
//! | `ALUFRAME [profile] [L W H] [corner]` | A box frame of twelve members with brackets at every joint. |
//! | `ALUCONNECT` | Add angle brackets where a profile end meets another profile's face (selection or whole drawing). |
//! | `ALULENGTH <mm>` | Change the length of the selected profiles (start and direction kept). |
//! | `ALUBOM [x,y]` | Insert the parts list as a table and print it. |
//! | `ALUCUTLIST [stock] [kerf]` | Optimised cut list per profile for stock bars (default 6000 mm, kerf 3 mm). |
//! | `ALUBOMCSV [path]` | Parts list and cut list as CSV. |
//! | `ALUCATALOG` | List the profiles. |

use super::{Message, OpenCADStudio};
use crate::command::{CadCommand, CmdOption, CmdResult};
use crate::scene::model::solid_model::{self, Bool};
use codec::objects::SolidHistoryOperation;
use codec::types::Vector3;
use codec::xdata::XDataValue;
use codec::{EntityType, Handle};
use glam::DVec3;
use iced::Task;
use kernel::brep::Body;
use kernel::geom2d::{Curve, Line};
use kernel::space::Plane;
use std::path::PathBuf;

/// Extended-data application name on every member.
pub const XDATA_APP: &str = "OCS_ALU";
/// Joint detection tolerance in millimetres.
const JOINT_TOL_MM: f64 = 0.5;

// ── Catalogue ────────────────────────────────────────────────────────────────

/// One profile of the catalogue. Dimensions in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileSpec {
    /// `40x40`, `40x80`, … (the keyword the commands accept).
    pub name: &'static str,
    pub part_no: &'static str,
    /// Cross-section across the profile's `u` axis.
    pub width: f64,
    /// Across `v`.
    pub height: f64,
    /// Nominal slot opening.
    pub slot: f64,
    /// Base module: one T-slot per `cell` of face length.
    pub cell: f64,
    /// Core bore diameter (for connectors and threads).
    pub bore: f64,
    pub kg_per_m: f64,
}

macro_rules! spec {
    ($name:literal, $part:literal, $w:literal x $h:literal, slot $slot:literal, cell $cell:literal, bore $bore:literal, $kg:literal) => {
        ProfileSpec {
            name: $name,
            part_no: $part,
            width: $w,
            height: $h,
            slot: $slot,
            cell: $cell,
            bore: $bore,
            kg_per_m: $kg,
        }
    };
}

/// Generic T-slot series (6 / 8 / 10 mm slots) with typical weights.
pub const CATALOG: &[ProfileSpec] = &[
    spec!("20x20", "AP-20-20", 20.0 x 20.0, slot 6.0, cell 20.0, bore 4.3, 0.42),
    spec!("20x40", "AP-20-40", 20.0 x 40.0, slot 6.0, cell 20.0, bore 4.3, 0.75),
    spec!("20x60", "AP-20-60", 20.0 x 60.0, slot 6.0, cell 20.0, bore 4.3, 1.08),
    spec!("20x80", "AP-20-80", 20.0 x 80.0, slot 6.0, cell 20.0, bore 4.3, 1.40),
    spec!("30x30", "AP-30-30", 30.0 x 30.0, slot 8.0, cell 30.0, bore 6.8, 0.90),
    spec!("30x60", "AP-30-60", 30.0 x 60.0, slot 8.0, cell 30.0, bore 6.8, 1.60),
    spec!("40x40", "AP-40-40", 40.0 x 40.0, slot 8.0, cell 40.0, bore 6.8, 1.50),
    spec!("40x80", "AP-40-80", 40.0 x 80.0, slot 8.0, cell 40.0, bore 6.8, 2.90),
    spec!("40x120", "AP-40-120", 40.0 x 120.0, slot 8.0, cell 40.0, bore 6.8, 4.30),
    spec!("40x160", "AP-40-160", 40.0 x 160.0, slot 8.0, cell 40.0, bore 6.8, 5.70),
    spec!("45x45", "AP-45-45", 45.0 x 45.0, slot 10.0, cell 45.0, bore 8.5, 1.90),
    spec!("45x90", "AP-45-90", 45.0 x 90.0, slot 10.0, cell 45.0, bore 8.5, 3.60),
    spec!("50x50", "AP-50-50", 50.0 x 50.0, slot 10.0, cell 50.0, bore 8.5, 2.30),
    spec!("50x100", "AP-50-100", 50.0 x 100.0, slot 10.0, cell 50.0, bore 8.5, 4.40),
    spec!("60x60", "AP-60-60", 60.0 x 60.0, slot 8.0, cell 30.0, bore 6.8, 2.80),
    spec!("80x80", "AP-80-80", 80.0 x 80.0, slot 8.0, cell 40.0, bore 6.8, 5.60),
    spec!("90x90", "AP-90-90", 90.0 x 90.0, slot 10.0, cell 45.0, bore 8.5, 6.00),
];

/// Default when the person just presses Enter.
pub const DEFAULT_PROFILE: &str = "40x40";

/// Look a profile up by name (`40x40`, `40X40`) or part number.
pub fn find_profile(text: &str) -> Option<&'static ProfileSpec> {
    let wanted = text.trim().to_ascii_lowercase().replace('*', "x").replace('×', "x");
    CATALOG
        .iter()
        .find(|p| p.name == wanted || p.part_no.eq_ignore_ascii_case(wanted.as_str()))
}

/// Angle bracket part for a slot series, by base module.
pub fn bracket_part(cell: f64) -> (String, String) {
    let size = cell.round() as i64;
    (format!("AB-{size}"), format!("Angle bracket {size}x{size}"))
}

// ── Cross-section ────────────────────────────────────────────────────────────

/// The closed outline of a profile in millimetres, centred on the origin and
/// wound counter-clockwise: the rectangle with one T-slot per cell on every
/// face and small corner chamfers.
pub fn outline(spec: &ProfileSpec) -> Vec<[f64; 2]> {
    let (w, h) = (spec.width, spec.height);
    let c = spec.cell;
    let lip = (0.045 * c).max(1.0);
    let cavity_w = 0.42 * c;
    let cavity_d = 0.15 * c;
    let chamfer = (0.05 * c).min(2.0);
    let slot = spec.slot;
    let mut points: Vec<[f64; 2]> = Vec::new();
    // Each side: (length, mapping from side-local (t, depth) to (u, v)).
    let sides: [(f64, Box<dyn Fn(f64, f64) -> [f64; 2]>); 4] = [
        (w, Box::new(move |t, d| [t, -h / 2.0 + d])),
        (h, Box::new(move |t, d| [w / 2.0 - d, t])),
        (w, Box::new(move |t, d| [-t, h / 2.0 - d])),
        (h, Box::new(move |t, d| [-w / 2.0 + d, -t])),
    ];
    for (len, map) in sides.iter() {
        let cells = (len / c).round().max(1.0) as usize;
        let cell = len / cells as f64;
        points.push(map(-len / 2.0 + chamfer, 0.0));
        for k in 0..cells {
            let tc = -len / 2.0 + cell * (k as f64 + 0.5);
            for (t, d) in [
                (tc - slot / 2.0, 0.0),
                (tc - slot / 2.0, lip),
                (tc - cavity_w / 2.0, lip),
                (tc - cavity_w / 2.0, lip + cavity_d),
                (tc + cavity_w / 2.0, lip + cavity_d),
                (tc + cavity_w / 2.0, lip),
                (tc + slot / 2.0, lip),
                (tc + slot / 2.0, 0.0),
            ] {
                points.push(map(t, d));
            }
        }
        points.push(map(len / 2.0 - chamfer, 0.0));
    }
    points
}

/// Line segments through `points` (closed), each scaled by `scale`.
fn polygon_curves(points: &[[f64; 2]], scale: f64) -> Vec<Curve> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let a = points[i];
            let b = points[(i + 1) % n];
            Curve::Line(Line {
                start: [a[0] * scale, a[1] * scale],
                end: [b[0] * scale, b[1] * scale],
            })
        })
        .collect()
}

/// Signed area of a 2D polygon (positive = counter-clockwise).
pub fn polygon_area(points: &[[f64; 2]]) -> f64 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let a = points[i];
            let b = points[(i + 1) % n];
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        * 0.5
}

/// The profile's `u` axis for an axis direction: horizontal members keep
/// their height vertical, vertical members take X as their reference.
pub fn frame_u(dir: DVec3) -> DVec3 {
    let up = if dir.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
    dir.cross(up).normalize_or(DVec3::Y)
}

/// Half extent of the cross-section along a unit direction `n` lying in the
/// cross plane (exact for the axis directions, conservative in between).
pub fn half_extent_along(spec: &ProfileSpec, dir: DVec3, n: DVec3) -> f64 {
    let u = frame_u(dir);
    let v = dir.cross(u);
    (n.dot(u).abs() * spec.width + n.dot(v).abs() * spec.height) * 0.5
}

/// The solid of one member: the outline extruded `length` along `dir` from
/// `start`, core bore subtracted. All lengths already in drawing units;
/// `scale` is drawing units per millimetre.
pub fn profile_body(spec: &ProfileSpec, start: DVec3, dir: DVec3, length: f64, scale: f64) -> Option<Body> {
    let dir = dir.normalize_or(DVec3::X);
    if !(length > 0.0) || !length.is_finite() {
        return None;
    }
    let u = frame_u(dir);
    let plane = Plane::orthonormal(start.to_array(), u.to_array(), dir.to_array())?;
    let curves = polygon_curves(&outline(spec), scale);
    let body = kernel::brep::extrude(plane, &curves, (dir * length).to_array())?;
    // The core bore, slightly longer than the member so the faces never
    // coincide; a failed boolean keeps the plain extrusion.
    let radius = spec.bore * scale * 0.5;
    let margin = scale * 0.5;
    let bore = solid_model::cylinder_solid([0.0, 0.0, -margin], radius, length + 2.0 * margin)
        .and_then(|cylinder| {
            let v = dir.cross(u);
            solid_model::placed(&cylinder, u.to_array(), v.to_array(), dir.to_array(), start.to_array())
        });
    match bore {
        Some(bore) => solid_model::boolean(Bool::Subtract, &body, &bore).or(Some(body)),
        None => Some(body),
    }
}

/// An angle bracket in the local frame x = into member A, y = along B away
/// from A's side, z = across: two plates of thickness `t` forming an L in the
/// x-y plane, hugging A's face at `y = face` and B's face at `x = 0`.
pub fn bracket_body(leg: f64, width: f64, t: f64, face: f64) -> Option<Body> {
    let on_a = solid_model::box_solid([leg * 0.5, face + t * 0.5, 0.0], leg, t, width)?;
    let on_b = solid_model::box_solid([t * 0.5, face + leg * 0.5, 0.0], t, leg, width)?;
    solid_model::boolean(Bool::Union, &on_a, &on_b).or(Some(on_a))
}

// ── Members in the drawing ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Profile,
    Connector,
}

/// One tagged solid.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub handle: Handle,
    pub kind: Kind,
    pub part_no: String,
    pub name: String,
    pub length_mm: f64,
    pub start: DVec3,
    pub dir: DVec3,
}

impl Member {
    pub fn spec(&self) -> Option<&'static ProfileSpec> {
        find_profile(&self.name).or_else(|| find_profile(&self.part_no))
    }
    pub fn end(&self, scale: f64) -> DVec3 {
        self.start + self.dir * (self.length_mm * scale)
    }
    pub fn mid(&self, scale: f64) -> DVec3 {
        self.start + self.dir * (self.length_mm * scale * 0.5)
    }
}

fn tag_values(kind: Kind, part_no: &str, name: &str, length_mm: f64, start: DVec3, dir: DVec3) -> Vec<XDataValue> {
    vec![
        XDataValue::String(match kind {
            Kind::Profile => "profile".into(),
            Kind::Connector => "connector".into(),
        }),
        XDataValue::String(part_no.to_string()),
        XDataValue::String(name.to_string()),
        XDataValue::Real(length_mm),
        XDataValue::Point3D(Vector3::new(start.x, start.y, start.z)),
        XDataValue::Direction3D(Vector3::new(dir.x, dir.y, dir.z)),
    ]
}

/// Decode a member tag; `None` for anything that is not one.
pub fn member_from_values(handle: Handle, values: &[XDataValue]) -> Option<Member> {
    let kind = match values.first()? {
        XDataValue::String(s) if s == "profile" => Kind::Profile,
        XDataValue::String(s) if s == "connector" => Kind::Connector,
        _ => return None,
    };
    let text = |index: usize| match values.get(index)? {
        XDataValue::String(s) => Some(s.clone()),
        _ => None,
    };
    let point = |index: usize| match values.get(index)? {
        XDataValue::Point3D(p) | XDataValue::Direction3D(p) | XDataValue::Position3D(p) => {
            Some(DVec3::new(p.x, p.y, p.z))
        }
        _ => None,
    };
    let length_mm = match values.get(3)? {
        XDataValue::Real(v) => *v,
        _ => return None,
    };
    Some(Member {
        handle,
        kind,
        part_no: text(1)?,
        name: text(2)?,
        length_mm,
        start: point(4)?,
        dir: point(5)?,
    })
}

/// Every tagged member of a document.
pub fn members_of(document: &codec::CadDocument) -> Vec<Member> {
    document
        .entities()
        .filter_map(|entity| {
            let common = entity.common();
            let record = common
                .extended_data
                .records()
                .iter()
                .find(|r| r.application_name == XDATA_APP)?;
            member_from_values(common.handle, &record.values)
        })
        .collect()
}

// ── Joints ───────────────────────────────────────────────────────────────────

/// A profile end resting on another profile's face.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    /// The member whose end touches.
    pub a: Handle,
    /// The member whose face is touched.
    pub b: Handle,
    /// Where A's axis meets B's face (drawing units).
    pub point: DVec3,
    /// Unit vector from the joint back into A.
    pub into_a: DVec3,
    /// Unit vector along B away from A's side.
    pub along_b: DVec3,
    /// Bracket module in millimetres.
    pub cell: f64,
    /// A's half extent along `along_b` (drawing units) — where its face is.
    pub face: f64,
}

/// Find end-to-face joints among `profiles`. `scale` = drawing units per mm.
pub fn find_joints(profiles: &[Member], scale: f64) -> Vec<Joint> {
    let tol = JOINT_TOL_MM * scale;
    let mut joints = Vec::new();
    for a in profiles {
        let Some(spec_a) = a.spec() else { continue };
        let a_dir = a.dir.normalize_or(DVec3::X);
        for (end, into_a) in [(a.start, a_dir), (a.end(scale), -a_dir)] {
            for b in profiles {
                if b.handle == a.handle {
                    continue;
                }
                let Some(spec_b) = b.spec() else { continue };
                let b_dir = b.dir.normalize_or(DVec3::X);
                if a_dir.dot(b_dir).abs() > 0.2 {
                    continue; // parallel members: no bracket joint
                }
                let len_b = b.length_mm * scale;
                let t = (end - b.start).dot(b_dir);
                if t < -tol || t > len_b + tol {
                    continue;
                }
                let foot = b.start + b_dir * t;
                let perp = end - foot;
                let dist = perp.length();
                if dist < tol {
                    continue;
                }
                let n = perp / dist;
                let half_b = half_extent_along(spec_b, b_dir, n);
                if (dist - half_b).abs() > tol {
                    continue;
                }
                // B continues on which side of the joint? Towards its middle.
                let toward_mid = (b.mid(scale) - end).dot(b_dir);
                let along_b = if toward_mid < -tol { -b_dir } else { b_dir };
                let cell = spec_a.cell.min(spec_b.cell);
                let face = half_extent_along(spec_a, a_dir, along_b);
                joints.push(Joint {
                    a: a.handle,
                    b: b.handle,
                    point: end,
                    into_a,
                    along_b,
                    cell,
                    face,
                });
            }
        }
    }
    joints
}

// ── Parts list and cut list ──────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct BomRow {
    pub part_no: String,
    pub name: String,
    /// Cut length in mm; 0 for connectors.
    pub length_mm: f64,
    pub qty: usize,
    pub kg: f64,
}

/// Group members into parts-list rows: profiles by part and length,
/// connectors by part.
pub fn bom_rows(members: &[Member]) -> Vec<BomRow> {
    let mut rows: Vec<BomRow> = Vec::new();
    for m in members {
        let length_mm = if m.kind == Kind::Profile { (m.length_mm * 10.0).round() / 10.0 } else { 0.0 };
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.part_no == m.part_no && (r.length_mm - length_mm).abs() < 1e-6)
        {
            row.qty += 1;
        } else {
            rows.push(BomRow { part_no: m.part_no.clone(), name: m.name.clone(), length_mm, qty: 1, kg: 0.0 });
        }
    }
    for row in &mut rows {
        let kg_per_m = find_profile(&row.name).or_else(|| find_profile(&row.part_no)).map(|s| s.kg_per_m).unwrap_or(0.0);
        row.kg = kg_per_m * row.length_mm / 1000.0 * row.qty as f64;
    }
    rows.sort_by(|a, b| {
        a.part_no
            .cmp(&b.part_no)
            .then(b.length_mm.partial_cmp(&a.length_mm).unwrap_or(std::cmp::Ordering::Equal))
    });
    rows
}

/// One stock bar of a cut plan.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub cuts: Vec<f64>,
    pub waste: f64,
}

/// First-fit-decreasing packing of `pieces` (mm) into bars of `stock` with a
/// saw `kerf` between cuts. Pieces longer than a bar get a bar of their own
/// and are reported as `waste < 0`.
pub fn cut_plan(pieces: &[f64], stock: f64, kerf: f64) -> Vec<Bar> {
    let mut sorted: Vec<f64> = pieces.iter().copied().filter(|p| *p > 0.0).collect();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut bars: Vec<(Vec<f64>, f64)> = Vec::new(); // (cuts, used)
    for piece in sorted {
        let slot = bars.iter_mut().find(|(cuts, used)| {
            let extra = if cuts.is_empty() { 0.0 } else { kerf };
            used + extra + piece <= stock + 1e-9
        });
        match slot {
            Some((cuts, used)) => {
                let extra = if cuts.is_empty() { 0.0 } else { kerf };
                *used += extra + piece;
                cuts.push(piece);
            }
            None => bars.push((vec![piece], piece)),
        }
    }
    bars.into_iter()
        .map(|(cuts, used)| Bar { waste: stock - used, cuts })
        .collect()
}

/// `(part_no, bars)` for every profile in the list.
pub fn cut_plans(rows: &[BomRow], stock: f64, kerf: f64) -> Vec<(String, Vec<Bar>)> {
    let mut parts: Vec<String> = rows.iter().filter(|r| r.length_mm > 0.0).map(|r| r.part_no.clone()).collect();
    parts.dedup();
    parts
        .into_iter()
        .map(|part| {
            let pieces: Vec<f64> = rows
                .iter()
                .filter(|r| r.part_no == part && r.length_mm > 0.0)
                .flat_map(|r| std::iter::repeat_n(r.length_mm, r.qty))
                .collect();
            let bars = cut_plan(&pieces, stock, kerf);
            (part, bars)
        })
        .collect()
}

fn csv_escape(text: &str) -> String {
    if text.contains([',', '"', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// Parts list followed by the cut plan, as CSV text.
pub fn csv_text(rows: &[BomRow], plans: &[(String, Vec<Bar>)], stock: f64) -> String {
    let mut out = String::from("Pos,Part no,Description,Length mm,Qty,Weight kg\n");
    for (i, row) in rows.iter().enumerate() {
        out.push_str(&format!(
            "{},{},{},{},{},{:.3}\n",
            i + 1,
            csv_escape(&row.part_no),
            csv_escape(&row.name),
            if row.length_mm > 0.0 { format!("{}", row.length_mm) } else { String::new() },
            row.qty,
            row.kg
        ));
    }
    out.push_str(&format!("\nCut list (stock {stock} mm)\nPart no,Bar,Cuts mm,Waste mm\n"));
    for (part, bars) in plans {
        for (index, bar) in bars.iter().enumerate() {
            out.push_str(&format!(
                "{},{},{},{:.1}\n",
                csv_escape(part),
                index + 1,
                csv_escape(&bar.cuts.iter().map(|c| format!("{c}")).collect::<Vec<_>>().join(" + ")),
                bar.waste
            ));
        }
    }
    out
}

fn parse_point(text: &str) -> Option<DVec3> {
    let parts: Vec<f64> = text.split(',').map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [x, y] => Some(DVec3::new(*x, *y, 0.0)),
        [x, y, z] => Some(DVec3::new(*x, *y, *z)),
        _ => None,
    }
}

fn parse_direction(text: &str) -> Option<DVec3> {
    match text.trim().to_ascii_uppercase().as_str() {
        "X" => Some(DVec3::X),
        "Y" => Some(DVec3::Y),
        "Z" => Some(DVec3::Z),
        "-X" => Some(-DVec3::X),
        "-Y" => Some(-DVec3::Y),
        "-Z" => Some(-DVec3::Z),
        _ => parse_point(text).filter(|d| d.length() > 1e-9).map(|d| d.normalize()),
    }
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn fmt_point(p: DVec3) -> String {
    format!("{},{},{}", fmt_num(p.x), fmt_num(p.y), fmt_num(p.z))
}

// ── Interactive wizard ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardKind {
    Profile,
    Frame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Spec,
    /// Length (profile) or length / width / height (frame), by index.
    Value(usize),
    Start,
    End,
}

/// Collects what `ALUPROFILE` / `ALUFRAME` were not given and dispatches
/// the complete command, so typed and clicked input run one code path.
pub struct AluWizard {
    kind: WizardKind,
    spec: Option<&'static ProfileSpec>,
    values: Vec<f64>,
    start: Option<DVec3>,
    step: Step,
    /// Millimetres per drawing unit, to turn a picked distance into mm.
    mm_per_unit: f64,
}

impl AluWizard {
    pub fn new(kind: WizardKind, spec: Option<&'static ProfileSpec>, values: Vec<f64>, mm_per_unit: f64) -> Self {
        let step = if spec.is_none() {
            Step::Spec
        } else {
            Self::first_value_step(kind, &values)
        };
        Self { kind, spec, values, start: None, step, mm_per_unit }
    }

    fn value_count(kind: WizardKind) -> usize {
        match kind {
            WizardKind::Profile => 1,
            WizardKind::Frame => 3,
        }
    }

    fn first_value_step(kind: WizardKind, values: &[f64]) -> Step {
        if values.len() < Self::value_count(kind) {
            Step::Value(values.len())
        } else {
            Step::Start
        }
    }

    fn advance_after_value(&mut self) {
        self.step = Self::first_value_step(self.kind, &self.values);
    }

    fn dispatch(&self, end: Option<DVec3>) -> CmdResult {
        let spec = self.spec.unwrap_or(&CATALOG[0]);
        let start = self.start.unwrap_or(DVec3::ZERO);
        match self.kind {
            WizardKind::Profile => {
                let (length_mm, dir) = match (self.values.first(), end) {
                    (Some(len), Some(end)) => (*len, (end - start).normalize_or(DVec3::X)),
                    (None, Some(end)) => (((end - start).length() * self.mm_per_unit), (end - start).normalize_or(DVec3::X)),
                    (Some(len), None) => (*len, DVec3::X),
                    (None, None) => return CmdResult::Cancel,
                };
                CmdResult::Dispatch(format!(
                    "ALUPROFILE {} {} {} {}",
                    spec.name,
                    fmt_num(length_mm),
                    fmt_point(start),
                    fmt_point(dir)
                ))
            }
            WizardKind::Frame => CmdResult::Dispatch(format!(
                "ALUFRAME {} {} {} {} {}",
                spec.name,
                fmt_num(self.values[0]),
                fmt_num(self.values[1]),
                fmt_num(self.values[2]),
                fmt_point(start)
            )),
        }
    }
}

impl CadCommand for AluWizard {
    fn name(&self) -> &'static str {
        match self.kind {
            WizardKind::Profile => "ALUPROFILE",
            WizardKind::Frame => "ALUFRAME",
        }
    }

    fn prompt(&self) -> String {
        match (self.step, self.kind) {
            (Step::Spec, _) => crate::tf!("Profile <{}>:", DEFAULT_PROFILE).into_owned(),
            (Step::Value(0), WizardKind::Profile) => crate::t!("Length in mm (Enter: pick the end point):").into_owned(),
            (Step::Value(0), WizardKind::Frame) => crate::t!("Frame length in mm:").into_owned(),
            (Step::Value(1), _) => crate::t!("Frame width in mm:").into_owned(),
            (Step::Value(_), _) => crate::t!("Frame height in mm:").into_owned(),
            (Step::Start, WizardKind::Profile) => crate::t!("Start point:").into_owned(),
            (Step::Start, WizardKind::Frame) => crate::t!("Corner point:").into_owned(),
            (Step::End, _) => crate::t!("End point (direction):").into_owned(),
        }
    }

    fn options(&self) -> Vec<CmdOption> {
        match self.step {
            Step::Spec => CATALOG.iter().map(|p| CmdOption::new(p.name, p.name)).collect(),
            Step::Value(0) if self.kind == WizardKind::Profile => vec![CmdOption::enter("End point")],
            _ => Vec::new(),
        }
    }

    fn wants_text_input(&self) -> bool {
        matches!(self.step, Step::Spec | Step::Value(_))
    }

    fn on_text_input(&mut self, text: &str) -> Option<CmdResult> {
        let text = text.trim();
        match self.step {
            Step::Spec => {
                let spec = if text.is_empty() { find_profile(DEFAULT_PROFILE) } else { find_profile(text) };
                match spec {
                    Some(spec) => {
                        self.spec = Some(spec);
                        self.step = Self::first_value_step(self.kind, &self.values);
                    }
                    None => return Some(CmdResult::NeedPoint),
                }
                Some(CmdResult::NeedPoint)
            }
            Step::Value(index) => {
                if text.is_empty() {
                    return Some(self.on_enter());
                }
                match text.parse::<f64>() {
                    Ok(v) if v.is_finite() && v > 0.0 => {
                        if index < self.values.len() {
                            self.values[index] = v;
                        } else {
                            self.values.push(v);
                        }
                        self.advance_after_value();
                        Some(CmdResult::NeedPoint)
                    }
                    _ => Some(CmdResult::NeedPoint),
                }
            }
            Step::Start | Step::End => {
                let point = parse_point(text)?;
                Some(self.on_point(point))
            }
        }
    }

    fn on_point(&mut self, pt: DVec3) -> CmdResult {
        match self.step {
            Step::Spec | Step::Value(_) => CmdResult::NeedPoint,
            Step::Start => {
                self.start = Some(pt);
                match self.kind {
                    WizardKind::Frame => self.dispatch(None),
                    WizardKind::Profile => {
                        self.step = Step::End;
                        CmdResult::NeedPoint
                    }
                }
            }
            Step::End => self.dispatch(Some(pt)),
        }
    }

    fn on_enter(&mut self) -> CmdResult {
        match self.step {
            Step::Spec => {
                self.spec = find_profile(DEFAULT_PROFILE);
                self.step = Self::first_value_step(self.kind, &self.values);
                CmdResult::NeedPoint
            }
            // No length: the end point decides it.
            Step::Value(0) if self.kind == WizardKind::Profile => {
                self.step = Step::Start;
                CmdResult::NeedPoint
            }
            Step::Value(_) => CmdResult::NeedPoint,
            Step::Start => CmdResult::Cancel,
            Step::End => match self.values.first() {
                // Length known, Enter = along X.
                Some(_) => self.dispatch(None),
                None => CmdResult::Cancel,
            },
        }
    }
}

// ── App integration ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AluMsg {
    CsvPicked(Option<PathBuf>),
}

impl OpenCADStudio {
    fn alu_scale(&self, i: usize) -> f64 {
        1.0 / super::print3d::drawing_unit_mm(self.tabs[i].scene.document.header.insertion_units)
    }

    fn alu_start(&mut self, i: usize, wizard: AluWizard) -> Task<Message> {
        self.reset_command_start_state(i);
        self.command_line.push_info(&wizard.prompt());
        self.tabs[i].active_cmd = Some(Box::new(wizard));
        self.push_ucs_to_cmd(i);
        self.sync_dyn_fields();
        self.focus_cmd_input()
    }

    /// Add one tagged solid. `length_mm` is only informative for connectors.
    fn alu_add_member(
        &mut self,
        i: usize,
        body: Body,
        kind: Kind,
        part_no: &str,
        name: &str,
        length_mm: f64,
        start: DVec3,
        dir: DVec3,
    ) -> Option<Handle> {
        let handle = self.add_solid_model(
            crate::modules::insert::solid3d_cmds::empty_solid3d(),
            body,
            SolidHistoryOperation::Unknown,
        );
        if handle.is_null() {
            return None;
        }
        crate::scene::view::dispatch::set_entity_xdata(
            &mut self.tabs[i].scene.document,
            handle,
            XDATA_APP,
            Some(tag_values(kind, part_no, name, length_mm, start, dir)),
        );
        self.tabs[i].dirty = true;
        Some(handle)
    }

    fn alu_place_profile(&mut self, i: usize, spec: &'static ProfileSpec, start: DVec3, dir: DVec3, length_mm: f64) -> Option<Handle> {
        let scale = self.alu_scale(i);
        let dir = dir.normalize_or(DVec3::X);
        let body = profile_body(spec, start, dir, length_mm * scale, scale)?;
        self.alu_add_member(i, body, Kind::Profile, spec.part_no, spec.name, length_mm, start, dir)
    }

    /// Brackets for `joints` that have none yet. Returns how many were added.
    fn alu_add_brackets(&mut self, i: usize, joints: &[Joint]) -> usize {
        let scale = self.alu_scale(i);
        let existing: Vec<DVec3> = members_of(&self.tabs[i].scene.document)
            .into_iter()
            .filter(|m| m.kind == Kind::Connector)
            .map(|m| m.start)
            .collect();
        let tol = JOINT_TOL_MM * scale;
        let mut added = 0;
        for joint in joints {
            let anchor = joint.point + joint.along_b * joint.face;
            if existing.iter().any(|p| (*p - anchor).length() < tol) {
                continue;
            }
            let c = joint.cell * scale;
            let Some(local) = bracket_body(c, c * 0.95, (0.1 * c).max(2.0 * scale), joint.face) else { continue };
            let z = joint.into_a.cross(joint.along_b).normalize_or(DVec3::Z);
            let Some(body) = solid_model::placed(
                &local,
                joint.into_a.to_array(),
                joint.along_b.to_array(),
                z.to_array(),
                joint.point.to_array(),
            ) else {
                continue;
            };
            let (part, name) = bracket_part(joint.cell);
            if self
                .alu_add_member(i, body, Kind::Connector, &part, &name, 0.0, anchor, joint.along_b)
                .is_some()
            {
                added += 1;
            }
        }
        added
    }

    /// Joints among the selected profiles (or all of them).
    fn alu_connect(&mut self, i: usize, only_selected: bool) -> usize {
        let scale = self.alu_scale(i);
        let selected = self.tabs[i].scene.selected_handles_in_order();
        let profiles: Vec<Member> = members_of(&self.tabs[i].scene.document)
            .into_iter()
            .filter(|m| m.kind == Kind::Profile)
            .filter(|m| !only_selected || selected.contains(&m.handle))
            .collect();
        let joints = find_joints(&profiles, scale);
        self.alu_add_brackets(i, &joints)
    }

    fn alu_frame(&mut self, i: usize, spec: &'static ProfileSpec, l: f64, w: f64, h: f64, corner: DVec3) -> (usize, usize) {
        let scale = self.alu_scale(i);
        let c = spec.width;
        let p = |x: f64, y: f64, z: f64| corner + DVec3::new(x, y, z) * scale;
        let inner_w = (w - 2.0 * c).max(1.0);
        let inner_h = (h - 2.0 * c).max(1.0);
        let members: Vec<(DVec3, DVec3, f64)> = vec![
            // X members, bottom and top
            (p(0.0, c / 2.0, c / 2.0), DVec3::X, l),
            (p(0.0, w - c / 2.0, c / 2.0), DVec3::X, l),
            (p(0.0, c / 2.0, h - c / 2.0), DVec3::X, l),
            (p(0.0, w - c / 2.0, h - c / 2.0), DVec3::X, l),
            // Y members between them
            (p(c / 2.0, c, c / 2.0), DVec3::Y, inner_w),
            (p(l - c / 2.0, c, c / 2.0), DVec3::Y, inner_w),
            (p(c / 2.0, c, h - c / 2.0), DVec3::Y, inner_w),
            (p(l - c / 2.0, c, h - c / 2.0), DVec3::Y, inner_w),
            // verticals
            (p(c / 2.0, c / 2.0, c), DVec3::Z, inner_h),
            (p(l - c / 2.0, c / 2.0, c), DVec3::Z, inner_h),
            (p(c / 2.0, w - c / 2.0, c), DVec3::Z, inner_h),
            (p(l - c / 2.0, w - c / 2.0, c), DVec3::Z, inner_h),
        ];
        let mut placed: Vec<Handle> = Vec::new();
        for (start, dir, length) in members {
            if let Some(handle) = self.alu_place_profile(i, spec, start, dir, length) {
                placed.push(handle);
            }
        }
        let profiles: Vec<Member> = members_of(&self.tabs[i].scene.document)
            .into_iter()
            .filter(|m| placed.contains(&m.handle))
            .collect();
        let joints = find_joints(&profiles, scale);
        let brackets = self.alu_add_brackets(i, &joints);
        (placed.len(), brackets)
    }

    fn alu_bom_table(&mut self, i: usize, rows: &[BomRow], at: DVec3) -> Option<Handle> {
        use codec::entities::Table;
        let scale = self.alu_scale(i);
        let mut table = Table::new(Vector3::new(at.x, at.y, at.z), rows.len() + 1, 6);
        let headers = ["Pos", "Part no", "Description", "Length mm", "Qty", "kg"];
        for (col, header) in headers.iter().enumerate() {
            if let Some(cell) = table.cell_mut(0, col) {
                cell.set_text(header);
            }
        }
        for (r, row) in rows.iter().enumerate() {
            let texts = [
                (r + 1).to_string(),
                row.part_no.clone(),
                row.name.clone(),
                if row.length_mm > 0.0 { fmt_num(row.length_mm) } else { "-".into() },
                row.qty.to_string(),
                format!("{:.2}", row.kg),
            ];
            for (col, text) in texts.iter().enumerate() {
                if let Some(cell) = table.cell_mut(r + 1, col) {
                    cell.set_text(text);
                }
            }
        }
        for (col, width) in [12.0, 30.0, 45.0, 25.0, 12.0, 18.0].iter().enumerate() {
            table.set_column_width(col, width * scale);
        }
        for r in 0..=rows.len() {
            table.set_row_height(r, 8.0 * scale);
        }
        self.commit_entity_handle(EntityType::Table(Box::new(table)))
    }

    /// The commands of this module; `None` when `cmd` is not one of them.
    pub(in crate::app) fn dispatch_alu(&mut self, cmd: &str, i: usize) -> Option<Task<Message>> {
        let (verb, rest) = match cmd.split_once(' ') {
            Some((v, r)) => (v, r.trim()),
            None => (cmd, ""),
        };
        let args: Vec<&str> = rest.split_whitespace().collect();
        let mm_per_unit = super::print3d::drawing_unit_mm(self.tabs[i].scene.document.header.insertion_units);
        match verb {
            "ALUPROFILE" | "ALU" => {
                let spec = args.first().and_then(|a| find_profile(a));
                if !args.is_empty() && spec.is_none() {
                    self.command_line.push_error(
                        crate::tf!("ALUPROFILE: unknown profile \"{}\". ALUCATALOG lists them.", args[0]).as_ref(),
                    );
                    return Some(Task::none());
                }
                let length = args.get(1).and_then(|a| a.parse::<f64>().ok()).filter(|v| *v > 0.0);
                let start = args.get(2).and_then(|a| parse_point(a));
                let dir = args.get(3).and_then(|a| parse_direction(a));
                if let (Some(spec), Some(length), Some(start), Some(dir)) = (spec, length, start, dir) {
                    self.push_undo_snapshot(i, "ALUPROFILE");
                    match self.alu_place_profile(i, spec, start, dir, length) {
                        Some(_) => self.command_line.push_output(
                            crate::tf!("ALUPROFILE: {} × {} mm placed.", spec.name, fmt_num(length)).as_ref(),
                        ),
                        None => self
                            .command_line
                            .push_error(crate::t!("ALUPROFILE: the solid could not be built.").as_ref()),
                    }
                    return Some(Task::none());
                }
                let wizard = AluWizard::new(WizardKind::Profile, spec, length.into_iter().collect(), mm_per_unit);
                Some(self.alu_start(i, wizard))
            }
            "ALUFRAME" => {
                let spec = args.first().and_then(|a| find_profile(a));
                if !args.is_empty() && spec.is_none() {
                    self.command_line.push_error(
                        crate::tf!("ALUPROFILE: unknown profile \"{}\". ALUCATALOG lists them.", args[0]).as_ref(),
                    );
                    return Some(Task::none());
                }
                let dims: Vec<f64> = args
                    .iter()
                    .skip(1)
                    .take(3)
                    .filter_map(|a| a.parse::<f64>().ok())
                    .filter(|v| *v > 0.0)
                    .collect();
                let corner = args.get(4).and_then(|a| parse_point(a));
                if let (Some(spec), 3, Some(corner)) = (spec, dims.len(), corner) {
                    self.push_undo_snapshot(i, "ALUFRAME");
                    let (members, brackets) = self.alu_frame(i, spec, dims[0], dims[1], dims[2], corner);
                    self.command_line.push_output(
                        crate::tf!("ALUFRAME: {} members and {} connectors placed.", members, brackets).as_ref(),
                    );
                    return Some(Task::none());
                }
                let wizard = AluWizard::new(WizardKind::Frame, spec, dims, mm_per_unit);
                Some(self.alu_start(i, wizard))
            }
            "ALUCONNECT" => {
                let only_selected = !self.tabs[i].scene.selected_handles_in_order().is_empty()
                    && !args.first().is_some_and(|a| a.eq_ignore_ascii_case("ALL"));
                self.push_undo_snapshot(i, "ALUCONNECT");
                let added = self.alu_connect(i, only_selected);
                if added == 0 {
                    self.command_line.push_info(
                        crate::t!("ALUCONNECT: no new joints found (profile ends must touch another profile's face).").as_ref(),
                    );
                } else {
                    self.command_line
                        .push_output(crate::tf!("ALUCONNECT: {} connector(s) added.", added).as_ref());
                }
                Some(Task::none())
            }
            "ALULENGTH" => {
                let Some(length) = args.first().and_then(|a| a.parse::<f64>().ok()).filter(|v| *v > 0.0) else {
                    self.command_line.push_error(
                        crate::t!("ALULENGTH: usage ALULENGTH <mm> with the profiles selected.").as_ref(),
                    );
                    return Some(Task::none());
                };
                let selected = self.tabs[i].scene.selected_handles_in_order();
                let targets: Vec<Member> = members_of(&self.tabs[i].scene.document)
                    .into_iter()
                    .filter(|m| m.kind == Kind::Profile && selected.contains(&m.handle))
                    .collect();
                if targets.is_empty() {
                    self.command_line.push_error(
                        crate::t!("ALULENGTH: usage ALULENGTH <mm> with the profiles selected.").as_ref(),
                    );
                    return Some(Task::none());
                }
                self.push_undo_snapshot(i, "ALULENGTH");
                let mut changed = 0;
                for member in targets {
                    let Some(spec) = member.spec() else { continue };
                    self.tabs[i].scene.erase_entities(&[member.handle]);
                    if self.alu_place_profile(i, spec, member.start, member.dir, length).is_some() {
                        changed += 1;
                    }
                }
                self.command_line.push_output(
                    crate::tf!("ALULENGTH: {} profile(s) set to {} mm.", changed, fmt_num(length)).as_ref(),
                );
                Some(Task::none())
            }
            "ALUBOM" => {
                let members = members_of(&self.tabs[i].scene.document);
                if members.is_empty() {
                    self.command_line
                        .push_error(crate::t!("ALUBOM: no aluminium profiles in this drawing.").as_ref());
                    return Some(Task::none());
                }
                let rows = bom_rows(&members);
                for (index, row) in rows.iter().enumerate() {
                    self.command_line.push_output(&format!(
                        "{:>3}  {:<10} {:<22} {:>8}  x{:<3} {:>7.2} kg",
                        index + 1,
                        row.part_no,
                        row.name,
                        if row.length_mm > 0.0 { fmt_num(row.length_mm) } else { "-".into() },
                        row.qty,
                        row.kg
                    ));
                }
                let profiles = members.iter().filter(|m| m.kind == Kind::Profile).count();
                let kg: f64 = rows.iter().map(|r| r.kg).sum();
                let at = args.first().and_then(|a| parse_point(a)).unwrap_or(DVec3::ZERO);
                self.push_undo_snapshot(i, "ALUBOM");
                let _ = self.alu_bom_table(i, &rows, at);
                self.command_line.push_output(
                    crate::tf!("ALUBOM: {} rows, {} profiles, {:.2} kg.", rows.len(), profiles, kg).as_ref(),
                );
                Some(Task::none())
            }
            "ALUCUTLIST" => {
                let stock = args.first().and_then(|a| a.parse::<f64>().ok()).filter(|v| *v > 0.0).unwrap_or(6000.0);
                let kerf = args.get(1).and_then(|a| a.parse::<f64>().ok()).filter(|v| *v >= 0.0).unwrap_or(3.0);
                let members = members_of(&self.tabs[i].scene.document);
                let rows = bom_rows(&members);
                let plans = cut_plans(&rows, stock, kerf);
                if plans.is_empty() {
                    self.command_line
                        .push_error(crate::t!("ALUBOM: no aluminium profiles in this drawing.").as_ref());
                    return Some(Task::none());
                }
                for (part, bars) in &plans {
                    let total: f64 = bars.len() as f64 * stock;
                    let waste: f64 = bars.iter().map(|b| b.waste.max(0.0)).sum();
                    self.command_line.push_output(
                        crate::tf!(
                            "ALUCUTLIST: {}: {} bar(s) of {} mm, waste {:.1} %",
                            part,
                            bars.len(),
                            fmt_num(stock),
                            if total > 0.0 { waste / total * 100.0 } else { 0.0 }
                        )
                        .as_ref(),
                    );
                    for (index, bar) in bars.iter().enumerate().take(40) {
                        self.command_line.push_output(&format!(
                            "    bar {:>2}: {}  (rest {})",
                            index + 1,
                            bar.cuts.iter().map(|c| fmt_num(*c)).collect::<Vec<_>>().join(" + "),
                            fmt_num(bar.waste)
                        ));
                    }
                }
                Some(Task::none())
            }
            "ALUBOMCSV" => {
                if let Some(path) = args.first() {
                    return Some(self.on_alu(AluMsg::CsvPicked(Some(PathBuf::from(path)))));
                }
                Some(Task::perform(
                    async {
                        crate::sys::file_dialog()
                            .set_title(crate::t!("Save parts list").as_ref())
                            .set_file_name("parts-list.csv")
                            .add_filter(crate::t!("CSV Files").as_ref(), &["csv"])
                            .add_filter(crate::t!("All Files").as_ref(), &["*"])
                            .save_file()
                            .await
                            .map(|h| crate::sys::handle_path(&h))
                    },
                    |path| Message::Alu(AluMsg::CsvPicked(path)),
                ))
            }
            "ALUCATALOG" => {
                for p in CATALOG {
                    self.command_line.push_output(&format!(
                        "{:<8} {:<10} slot {:>2} mm  bore {:>4} mm  {:>5.2} kg/m",
                        p.name, p.part_no, fmt_num(p.slot), fmt_num(p.bore), p.kg_per_m
                    ));
                }
                self.command_line.push_output(
                    crate::tf!("ALUCATALOG: {} profiles. ALUPROFILE <name> <length> <x,y,z> <X|Y|Z|dx,dy,dz>", CATALOG.len()).as_ref(),
                );
                Some(Task::none())
            }
            _ => None,
        }
    }

    pub(in crate::app) fn on_alu(&mut self, msg: AluMsg) -> Task<Message> {
        match msg {
            AluMsg::CsvPicked(None) => Task::none(),
            AluMsg::CsvPicked(Some(path)) => {
                let i = self.active_tab;
                let members = members_of(&self.tabs[i].scene.document);
                let rows = bom_rows(&members);
                let plans = cut_plans(&rows, 6000.0, 3.0);
                let text = csv_text(&rows, &plans, 6000.0);
                match std::fs::write(&path, text) {
                    Ok(()) => self
                        .command_line
                        .push_output(crate::tf!("ALUBOMCSV: written \"{}\"", path.display()).as_ref()),
                    Err(error) => self.command_line.push_error(crate::tf!("ALUBOMCSV: {}", error).as_ref()),
                }
                Task::none()
            }
        }
    }
}

inventory::submit!(crate::command::CommandRegistration {
    names: &[
        "ALUPROFILE", "ALU", "ALUFRAME", "ALUCONNECT", "ALULENGTH", "ALUBOM", "ALUCUTLIST", "ALUBOMCSV", "ALUCATALOG",
    ]
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_lookup_and_outline_geometry() {
        assert_eq!(find_profile("40X40").unwrap().part_no, "AP-40-40");
        assert_eq!(find_profile("ap-20-40").unwrap().name, "20x40");
        assert!(find_profile("99x99").is_none());
        let spec = find_profile("40x80").unwrap();
        let pts = outline(spec);
        // 2 cells on the long faces, 1 on the short ones: 6 slots × 8 points + 8 chamfer points.
        assert_eq!(pts.len(), 6 * 8 + 8);
        let area = polygon_area(&pts);
        assert!(area > 0.0, "counter-clockwise");
        assert!(area < 40.0 * 80.0 && area > 0.8 * 40.0 * 80.0, "{area}");
        for p in &pts {
            assert!(p[0].abs() <= 20.0 + 1e-9 && p[1].abs() <= 40.0 + 1e-9);
        }
        let square = outline(find_profile("20x20").unwrap());
        assert_eq!(square.len(), 4 * 8 + 8);
    }

    #[test]
    fn member_tags_round_trip_and_extents_follow_the_frame() {
        let values = tag_values(Kind::Profile, "AP-40-80", "40x80", 1234.5, DVec3::new(1.0, 2.0, 3.0), DVec3::Y);
        let member = member_from_values(Handle::new(7), &values).unwrap();
        assert_eq!(member.kind, Kind::Profile);
        assert_eq!(member.spec().unwrap().name, "40x80");
        assert_eq!(member.length_mm, 1234.5);
        assert_eq!(member.end(1.0), DVec3::new(1.0, 1236.5, 3.0));
        assert!(member_from_values(Handle::new(7), &[XDataValue::Real(1.0)]).is_none());
        let spec = find_profile("40x80").unwrap();
        // Along X the width (40) spans Y and the height (80) spans Z.
        assert!((half_extent_along(spec, DVec3::X, DVec3::Z) - 40.0).abs() < 1e-9);
        assert!((half_extent_along(spec, DVec3::X, DVec3::Y) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn joints_are_found_where_an_end_meets_a_face() {
        let spec = find_profile("40x40").unwrap();
        let member = |handle: u64, start: DVec3, dir: DVec3, len: f64| Member {
            handle: Handle::new(handle),
            kind: Kind::Profile,
            part_no: spec.part_no.into(),
            name: spec.name.into(),
            length_mm: len,
            start,
            dir,
        };
        // Vertical post B at the origin; horizontal A ends on its +X face.
        let b = member(1, DVec3::ZERO, DVec3::Z, 500.0);
        let a = member(2, DVec3::new(20.0, 0.0, 250.0), DVec3::X, 300.0);
        let joints = find_joints(&[a.clone(), b.clone()], 1.0);
        assert_eq!(joints.len(), 1, "{joints:?}");
        let j = &joints[0];
        assert_eq!(j.a, a.handle);
        assert_eq!(j.b, b.handle);
        assert_eq!(j.into_a, DVec3::X);
        assert_eq!(j.cell, 40.0);
        assert!((j.face - 20.0).abs() < 1e-9);
        // A member floating 5 mm away is not joined; a parallel one never is.
        let far = member(3, DVec3::new(25.0, 0.0, 250.0), DVec3::X, 300.0);
        assert!(find_joints(&[far, b.clone()], 1.0).is_empty());
        let parallel = member(4, DVec3::new(40.0, 0.0, 0.0), DVec3::Z, 500.0);
        assert!(find_joints(&[parallel, b], 1.0).is_empty());
    }

    #[test]
    fn parts_list_groups_by_part_and_length_and_cuts_pack_bars() {
        let spec = find_profile("40x40").unwrap();
        let m = |handle: u64, len: f64| Member {
            handle: Handle::new(handle),
            kind: Kind::Profile,
            part_no: spec.part_no.into(),
            name: spec.name.into(),
            length_mm: len,
            start: DVec3::ZERO,
            dir: DVec3::X,
        };
        let members = vec![m(1, 1000.0), m(2, 1000.0), m(3, 500.0), Member {
            handle: Handle::new(4),
            kind: Kind::Connector,
            part_no: "AB-40".into(),
            name: "Angle bracket 40x40".into(),
            length_mm: 0.0,
            start: DVec3::ZERO,
            dir: DVec3::X,
        }];
        let rows = bom_rows(&members);
        assert_eq!(rows.len(), 3);
        let thousand = rows.iter().find(|r| r.length_mm == 1000.0).unwrap();
        assert_eq!(thousand.qty, 2);
        assert!((thousand.kg - 3.0).abs() < 1e-9);
        assert!(rows.iter().any(|r| r.part_no == "AB-40" && r.qty == 1 && r.length_mm == 0.0));
        let plan = cut_plan(&[3000.0, 3000.0, 2000.0, 1000.0, 1000.0], 6000.0, 3.0);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].cuts, vec![3000.0, 2000.0]); // 3000 + 3 + 2000 = 5003, no room for 1000 + kerf
        assert_eq!(plan[1].cuts, vec![3000.0, 1000.0, 1000.0]);
        let plans = cut_plans(&rows, 6000.0, 3.0);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].1.len(), 1);
        let csv = csv_text(&rows, &plans, 6000.0);
        assert!(csv.starts_with("Pos,Part no,Description,Length mm,Qty,Weight kg\n"));
        assert!(csv.contains("AP-40-40,40x40,1000,2,3.000"));
        assert!(csv.contains("Cut list (stock 6000 mm)"));
    }

    #[test]
    fn wizard_collects_profile_length_and_points_then_dispatches() {
        let mut w = AluWizard::new(WizardKind::Profile, None, Vec::new(), 1.0);
        assert!(w.wants_text_input());
        assert_eq!(w.options().len(), CATALOG.len());
        assert!(matches!(w.on_text_input("40x80"), Some(CmdResult::NeedPoint)));
        assert!(w.prompt().contains("Length"));
        assert!(matches!(w.on_text_input("600"), Some(CmdResult::NeedPoint)));
        assert!(!w.wants_text_input());
        assert!(matches!(w.on_point(DVec3::new(10.0, 0.0, 0.0)), CmdResult::NeedPoint));
        match w.on_point(DVec3::new(10.0, 5.0, 0.0)) {
            CmdResult::Dispatch(cmd) => assert_eq!(cmd, "ALUPROFILE 40x80 600 10,0,0 0,1,0"),
            other => panic!("unexpected {:?}", std::mem::discriminant(&other)),
        }
        // Enter at the length step: the end point sets the length (in mm from drawing units).
        let mut w = AluWizard::new(WizardKind::Profile, find_profile("20x20"), Vec::new(), 25.4);
        assert!(matches!(w.on_enter(), CmdResult::NeedPoint));
        w.on_point(DVec3::ZERO);
        match w.on_point(DVec3::new(0.0, 0.0, 10.0)) {
            CmdResult::Dispatch(cmd) => assert_eq!(cmd, "ALUPROFILE 20x20 254 0,0,0 0,0,1"),
            _ => panic!("expected dispatch"),
        }
        // Frame: three values then a corner.
        let mut f = AluWizard::new(WizardKind::Frame, find_profile("40x40"), vec![1000.0], 1.0);
        assert!(f.prompt().contains("width"));
        f.on_text_input("500");
        f.on_text_input("700");
        match f.on_point(DVec3::new(1.0, 2.0, 0.0)) {
            CmdResult::Dispatch(cmd) => assert_eq!(cmd, "ALUFRAME 40x40 1000 500 700 1,2,0"),
            _ => panic!("expected dispatch"),
        }
    }

    #[test]
    fn profile_and_bracket_solids_build() {
        let spec = find_profile("40x40").unwrap();
        let body = profile_body(spec, DVec3::ZERO, DVec3::X, 100.0, 1.0).expect("profile solid");
        let (min, max) = solid_model::extent(&body).expect("bounds");
        assert!((max[0] - min[0] - 100.0).abs() < 1e-6, "{min:?} {max:?}");
        assert!((max[1] - min[1] - 40.0).abs() < 1e-6);
        assert!((max[2] - min[2] - 40.0).abs() < 1e-6);
        assert!(profile_body(spec, DVec3::ZERO, DVec3::X, 0.0, 1.0).is_none());
        let bracket = bracket_body(40.0, 38.0, 4.0, 20.0).expect("bracket");
        let (min, max) = solid_model::extent(&bracket).expect("bounds");
        assert!((max[0] - 40.0).abs() < 1e-6 && min[0].abs() < 1e-6);
        assert!((min[1] - 20.0).abs() < 1e-6 && (max[1] - 60.0).abs() < 1e-6);
    }

    #[test]
    fn commands_place_profiles_and_list_them() {
        let mut app = OpenCADStudio::new();
        app.automation_op(r#"{"op":"new"}"#);
        assert!(app.dispatch_alu("LINE", 0).is_none());
        assert!(app.dispatch_alu("ALUPROFILE 40x40 500 0,0,0 X", 0).is_some());
        assert!(app.dispatch_alu("ALUPROFILE 40x40 300 520,0,0 Z", 0).is_some());
        let members = members_of(&app.tabs[0].scene.document);
        assert_eq!(members.len(), 2, "{members:?}");
        assert!(app.dispatch_alu("ALUPROFILE 99x99 500 0,0,0 X", 0).is_some());
        assert_eq!(members_of(&app.tabs[0].scene.document).len(), 2);
        assert!(app.dispatch_alu("ALUFRAME 40x40 600 400 300 0,0,1000", 0).is_some());
        let members = members_of(&app.tabs[0].scene.document);
        let profiles = members.iter().filter(|m| m.kind == Kind::Profile).count();
        let connectors = members.iter().filter(|m| m.kind == Kind::Connector).count();
        assert_eq!(profiles, 14);
        assert!(connectors >= 12, "frame joints get brackets: {connectors}");
        assert!(app.dispatch_alu("ALUBOM 0,-100", 0).is_some());
        assert!(app.tabs[0].scene.document.entities().any(|e| matches!(e, EntityType::Table(_))));
        assert!(app.dispatch_alu("ALUCUTLIST 3000 3", 0).is_some());
        assert!(app.dispatch_alu("ALUCATALOG", 0).is_some());
        // Starting without arguments opens the wizard.
        assert!(app.dispatch_alu("ALUPROFILE", 0).is_some());
        assert!(app.tabs[0].active_cmd.is_some());
    }
}
