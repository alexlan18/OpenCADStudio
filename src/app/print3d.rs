//! 3D printing: mesh exchange with slicers and a bridge to UltiMaker Cura.
//!
//! Cura is a Python/Qt application with its own C++ engine; its sources
//! cannot be merged into this Rust code base. What *can* be one workflow is
//! the hand-over: model here, import meshes from any slicer-side file
//! (STL / 3MF / OBJ), export the drawing's solids as 3MF or STL, open them in
//! an installed Cura with one command, or slice them in place through
//! Cura's own engine (`CuraEngine`, shipped inside every Cura installation)
//! and save the G-code — no second application window needed for the
//! common case.
//!
//! Commands (all reachable from the ribbon, the command line and the
//! automation API):
//!
//! | Command | What it does |
//! |---|---|
//! | `MESHIMPORT` (`IMPORTMESH`, `STLIMPORT`, `3MFIMPORT`) | Import an STL / 3MF / OBJ mesh into the drawing, scaled to its units. |
//! | `3MFOUT` (`EXPORT3MF`) | Export every mesh as one 3MF (millimetres). |
//! | `CURA` (`SENDTOCURA`, `PRINT3D`) | Export a 3MF to a temporary folder and open it in Cura. |
//! | `CURAPATH` | Pick the Cura executable; CuraEngine and the printer definitions are located next to it. |
//! | `GCODE` (`SLICE3D`, `CURASLICE`) | Slice the meshes with CuraEngine and save the G-code. (`SLICE` is the CAD command that cuts solids.) |
//! | `GCODESETTINGS` / `GCODESET name value` | Show / change the slicing profile. |

use super::{Message, OpenCADStudio};
use crate::scene::model::mesh_model::MeshModel;
use iced::Task;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Mesh colour for imported geometry (matches OBJ import).
const IMPORT_COLOR: [f32; 4] = [0.7, 0.7, 0.85, 1.0];

// ── Settings ─────────────────────────────────────────────────────────────────

/// Bed adhesion helper CuraEngine should add.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adhesion {
    None,
    #[default]
    Skirt,
    Brim,
    Raft,
}

impl Adhesion {
    pub fn wire(self) -> &'static str {
        match self {
            Adhesion::None => "none",
            Adhesion::Skirt => "skirt",
            Adhesion::Brim => "brim",
            Adhesion::Raft => "raft",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "none" | "0" => Some(Adhesion::None),
            "skirt" => Some(Adhesion::Skirt),
            "brim" => Some(Adhesion::Brim),
            "raft" => Some(Adhesion::Raft),
            _ => None,
        }
    }
}

/// The slicing profile handed to CuraEngine as `-s key=value` overrides on
/// top of Cura's generic `fdmprinter.def.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SliceSettings {
    pub layer_height: f64,
    /// Infill density in percent.
    pub infill: f64,
    pub walls: u32,
    pub top_bottom_layers: u32,
    pub nozzle_temp: f64,
    pub bed_temp: f64,
    /// Print speed in mm/s.
    pub speed: f64,
    pub support: bool,
    pub adhesion: Adhesion,
    pub bed_width: f64,
    pub bed_depth: f64,
    pub bed_height: f64,
    pub nozzle: f64,
    pub filament_diameter: f64,
}

impl Default for SliceSettings {
    fn default() -> Self {
        Self {
            layer_height: 0.2,
            infill: 20.0,
            walls: 2,
            top_bottom_layers: 4,
            nozzle_temp: 210.0,
            bed_temp: 60.0,
            speed: 50.0,
            support: false,
            adhesion: Adhesion::Skirt,
            bed_width: 220.0,
            bed_depth: 220.0,
            bed_height: 250.0,
            nozzle: 0.4,
            filament_diameter: 1.75,
        }
    }
}

impl SliceSettings {
    pub const NAMES: [&'static str; 14] = [
        "layer_height",
        "infill",
        "walls",
        "top_bottom_layers",
        "nozzle_temp",
        "bed_temp",
        "speed",
        "support",
        "adhesion",
        "bed_width",
        "bed_depth",
        "bed_height",
        "nozzle",
        "filament_diameter",
    ];

    /// `name = value` lines for `GCODESETTINGS`.
    pub fn describe(&self) -> String {
        format!(
            "layer_height={} infill={}% walls={} top_bottom_layers={} nozzle_temp={} bed_temp={} speed={} support={} adhesion={} bed_width={} bed_depth={} bed_height={} nozzle={} filament_diameter={}",
            self.layer_height,
            self.infill,
            self.walls,
            self.top_bottom_layers,
            self.nozzle_temp,
            self.bed_temp,
            self.speed,
            if self.support { "on" } else { "off" },
            self.adhesion.wire(),
            self.bed_width,
            self.bed_depth,
            self.bed_height,
            self.nozzle,
            self.filament_diameter
        )
    }

    /// Apply `GCODESET name value`.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let num = || -> Result<f64, String> {
            value
                .trim()
                .trim_end_matches('%')
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("invalid value {value:?}"))
        };
        match name.to_ascii_lowercase().as_str() {
            "layer_height" => self.layer_height = num()?.clamp(0.02, 2.0),
            "infill" => self.infill = num()?.clamp(0.0, 100.0),
            "walls" => self.walls = num()?.clamp(1.0, 20.0) as u32,
            "top_bottom_layers" => self.top_bottom_layers = num()?.clamp(0.0, 50.0) as u32,
            "nozzle_temp" => self.nozzle_temp = num()?.clamp(0.0, 400.0),
            "bed_temp" => self.bed_temp = num()?.clamp(0.0, 150.0),
            "speed" => self.speed = num()?.clamp(1.0, 500.0),
            "support" => {
                self.support = match value.trim().to_ascii_lowercase().as_str() {
                    "1" | "on" | "yes" | "true" => true,
                    "0" | "off" | "no" | "false" => false,
                    _ => return Err(format!("invalid value {value:?}")),
                }
            }
            "adhesion" => self.adhesion = Adhesion::parse(value.trim()).ok_or_else(|| format!("invalid value {value:?}"))?,
            "bed_width" => self.bed_width = num()?.clamp(10.0, 5000.0),
            "bed_depth" => self.bed_depth = num()?.clamp(10.0, 5000.0),
            "bed_height" => self.bed_height = num()?.clamp(10.0, 5000.0),
            "nozzle" => self.nozzle = num()?.clamp(0.1, 2.0),
            "filament_diameter" => self.filament_diameter = num()?.clamp(1.0, 3.5),
            _ => return Err(format!("unknown setting {name:?}")),
        }
        Ok(())
    }

    /// The `-s key=value` arguments for CuraEngine.
    pub fn engine_overrides(&self) -> Vec<String> {
        let line_width = self.nozzle;
        let wall_thickness = line_width * f64::from(self.walls);
        let top_bottom = self.layer_height * f64::from(self.top_bottom_layers);
        [
            format!("layer_height={}", self.layer_height),
            format!("layer_height_0={}", self.layer_height),
            format!("line_width={line_width}"),
            format!("wall_thickness={wall_thickness}"),
            format!("wall_line_count={}", self.walls),
            format!("top_bottom_thickness={top_bottom}"),
            format!("top_layers={}", self.top_bottom_layers),
            format!("bottom_layers={}", self.top_bottom_layers),
            format!("infill_sparse_density={}", self.infill),
            format!("material_print_temperature={}", self.nozzle_temp),
            format!("material_print_temperature_layer_0={}", self.nozzle_temp),
            format!("material_bed_temperature={}", self.bed_temp),
            format!("material_bed_temperature_layer_0={}", self.bed_temp),
            format!("speed_print={}", self.speed),
            format!("support_enable={}", self.support),
            format!("adhesion_type={}", self.adhesion.wire()),
            format!("machine_width={}", self.bed_width),
            format!("machine_depth={}", self.bed_depth),
            format!("machine_height={}", self.bed_height),
            format!("machine_nozzle_size={}", self.nozzle),
            format!("material_diameter={}", self.filament_diameter),
            // Put the model on the plate centre: drawings are modelled at the
            // origin, not on a printer bed.
            "center_object=true".to_string(),
            "mesh_position_x=0".to_string(),
            "mesh_position_y=0".to_string(),
            "mesh_position_z=0".to_string(),
        ]
        .into_iter()
        .flat_map(|kv| ["-s".to_string(), kv])
        .collect()
    }
}

/// Persisted 3D-print preferences (`UserSettings::print3d`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Print3dSettings {
    /// Cura executable; empty means auto-detect.
    pub cura_path: String,
    /// CuraEngine executable; empty means next to Cura or on PATH.
    pub engine_path: String,
    /// Folder holding `fdmprinter.def.json`; empty means inside the Cura install.
    pub definitions_dir: String,
    pub slice: SliceSettings,
}

// ── Locating Cura ────────────────────────────────────────────────────────────

/// A Cura installation as far as this application needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CuraInstall {
    pub cura: Option<PathBuf>,
    pub engine: Option<PathBuf>,
    pub definitions: Option<PathBuf>,
}

/// Executable names Cura has shipped under.
const CURA_EXE_NAMES: [&str; 4] = ["UltiMaker-Cura.exe", "Ultimaker-Cura.exe", "Cura.exe", "cura.exe"];

/// Where Cura may be installed on this platform, most likely first.
pub fn cura_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "windows") {
        for root in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            let Some(base) = std::env::var_os(root) else { continue };
            let base = PathBuf::from(base);
            let dirs: Vec<PathBuf> = std::fs::read_dir(&base)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                                .is_some_and(|n| n.contains("cura"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut dirs = dirs;
            dirs.sort();
            dirs.reverse(); // newest version name first
            for dir in dirs {
                for exe in CURA_EXE_NAMES {
                    out.push(dir.join(exe));
                }
            }
        }
    } else if cfg!(target_os = "macos") {
        for app in ["UltiMaker Cura.app", "Ultimaker Cura.app", "Cura.app"] {
            for base in ["/Applications", "~/Applications"] {
                let base = if let Some(rest) = base.strip_prefix('~') {
                    match std::env::var_os("HOME") {
                        Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
                        None => continue,
                    }
                } else {
                    PathBuf::from(base)
                };
                let macos = base.join(app).join("Contents/MacOS");
                out.push(macos.join("UltiMaker-Cura"));
                out.push(macos.join("Ultimaker-Cura"));
                out.push(macos.join("cura"));
            }
        }
    } else {
        for name in ["cura", "ultimaker-cura", "UltiMaker-Cura"] {
            if let Some(found) = which(name) {
                out.push(found);
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for dir in [home.join("Applications"), home.join("Downloads"), home.clone()] {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    let mut images: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                            name.contains("cura") && name.ends_with(".appimage")
                        })
                        .collect();
                    images.sort();
                    images.reverse();
                    out.extend(images);
                }
            }
        }
    }
    out
}

/// First executable on `PATH` with this name.
pub fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// CuraEngine and the printer definitions that ship next to a Cura
/// executable, following each platform's install layout.
pub fn engine_beside(cura: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(dir) = cura.parent() else { return (None, None) };
    let mut engines = vec![dir.join("CuraEngine.exe"), dir.join("CuraEngine")];
    let mut definitions = vec![
        dir.join("share/cura/resources/definitions"),
        dir.join("resources/definitions"),
        dir.join("share/cura/definitions"),
    ];
    // macOS bundle: Contents/MacOS/<exe>, Contents/Resources/share/cura/…
    if let Some(contents) = dir.parent().filter(|p| p.file_name().is_some_and(|n| n == "Contents")) {
        engines.push(contents.join("MacOS/CuraEngine"));
        definitions.push(contents.join("Resources/share/cura/resources/definitions"));
        definitions.push(contents.join("Resources/resources/definitions"));
    }
    let engine = engines.into_iter().find(|p| p.is_file());
    let definitions = definitions
        .into_iter()
        .find(|p| p.join("fdmprinter.def.json").is_file());
    (engine, definitions)
}

/// Resolve the installation from the settings, falling back to detection.
pub fn locate(settings: &Print3dSettings) -> CuraInstall {
    let cura = Some(PathBuf::from(settings.cura_path.trim()))
        .filter(|p| !settings.cura_path.trim().is_empty() && p.is_file())
        .or_else(|| cura_candidates().into_iter().find(|p| p.is_file()));
    let (mut engine, mut definitions) = cura.as_deref().map(engine_beside).unwrap_or((None, None));
    if !settings.engine_path.trim().is_empty() {
        let p = PathBuf::from(settings.engine_path.trim());
        if p.is_file() {
            engine = Some(p);
        }
    }
    if engine.is_none() {
        engine = which("CuraEngine").or_else(|| which("CuraEngine.exe"));
    }
    if !settings.definitions_dir.trim().is_empty() {
        let p = PathBuf::from(settings.definitions_dir.trim());
        if p.join("fdmprinter.def.json").is_file() {
            definitions = Some(p);
        }
    }
    if definitions.is_none() {
        definitions = ["/usr/share/cura/resources/definitions", "/usr/share/cura/definitions", "/usr/local/share/cura/resources/definitions"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.join("fdmprinter.def.json").is_file());
    }
    CuraInstall { cura, engine, definitions }
}

// ── Slicing ──────────────────────────────────────────────────────────────────

/// The full CuraEngine command line for one slice.
pub fn engine_args(definitions: &Path, settings: &SliceSettings, mesh: &Path, gcode: &Path) -> Vec<String> {
    let mut args = vec![
        "slice".to_string(),
        "-v".to_string(),
        "-j".to_string(),
        definitions.join("fdmprinter.def.json").to_string_lossy().into_owned(),
    ];
    args.extend(settings.engine_overrides());
    args.push("-l".to_string());
    args.push(mesh.to_string_lossy().into_owned());
    args.push("-o".to_string());
    args.push(gcode.to_string_lossy().into_owned());
    args
}

/// What a slice produced, read from the G-code header CuraEngine writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SliceReport {
    pub print_seconds: Option<u64>,
    pub filament: Option<String>,
    pub layers: Option<u64>,
    pub bytes: u64,
}

impl SliceReport {
    pub fn print_time(&self) -> String {
        match self.print_seconds {
            Some(s) => {
                let h = s / 3600;
                let m = (s % 3600) / 60;
                if h > 0 {
                    format!("{h}h {m:02}m")
                } else {
                    format!("{m}m {:02}s", s % 60)
                }
            }
            None => "?".into(),
        }
    }
}

/// Parse `;TIME:`, `;Filament used:` and `;LAYER_COUNT:` from a G-code head.
pub fn parse_gcode_header(head: &str, bytes: u64) -> SliceReport {
    let mut report = SliceReport { bytes, ..Default::default() };
    for line in head.lines().take(400) {
        if let Some(v) = line.strip_prefix(";TIME:") {
            report.print_seconds = v.trim().parse::<f64>().ok().map(|s| s.round() as u64);
        } else if let Some(v) = line.strip_prefix(";Filament used:") {
            report.filament = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix(";LAYER_COUNT:") {
            report.layers = v.trim().parse().ok();
        }
    }
    report
}

/// Run CuraEngine to completion (blocking; call from a background task).
#[cfg(not(target_arch = "wasm32"))]
pub fn run_engine(engine: &Path, args: &[String], gcode: &Path) -> Result<SliceReport, String> {
    let output = std::process::Command::new(engine)
        .args(args)
        .output()
        .map_err(|e| format!("cannot start CuraEngine at {}: {e}", engine.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("CuraEngine exited with {}: {}", output.status, tail.join(" | ")));
    }
    let bytes = std::fs::metadata(gcode).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return Err("CuraEngine wrote no G-code (is the model inside the bed?)".into());
    }
    let mut head = vec![0u8; 16 * 1024];
    let read = std::fs::File::open(gcode)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    head.truncate(read);
    Ok(parse_gcode_header(&String::from_utf8_lossy(&head), bytes))
}

// ── Mesh exchange ────────────────────────────────────────────────────────────

/// Millimetres per drawing unit from `INSUNITS` (unitless counts as mm).
pub fn drawing_unit_mm(insunits: i16) -> f64 {
    super::properties::insunits_to_mm(insunits).unwrap_or(1.0)
}

/// Read any supported mesh file into meshes in millimetres… or rather in
/// its own unit: the returned factor converts those coordinates to mm.
pub fn read_mesh_file(path: &Path) -> Result<(Vec<MeshModel>, f64), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match ext.as_str() {
        "3mf" => {
            let parsed = crate::io::threemf::parse_3mf(&bytes, IMPORT_COLOR)?;
            let factor = crate::io::threemf::unit_to_mm(&parsed.unit);
            Ok((parsed.meshes, factor))
        }
        "obj" => {
            let text = String::from_utf8_lossy(&bytes);
            let mesh = crate::io::obj::parse_obj(&text, IMPORT_COLOR).ok_or("no usable geometry in file")?;
            Ok((vec![mesh], 1.0))
        }
        _ => {
            let mesh = crate::io::stl::parse_stl(&bytes, IMPORT_COLOR).ok_or("no usable geometry in file")?;
            Ok((vec![mesh], 1.0))
        }
    }
}

/// Scale every vertex in place.
pub fn scale_mesh(mesh: &mut MeshModel, factor: f64) {
    if (factor - 1.0).abs() < 1e-12 {
        return;
    }
    for v in &mut mesh.verts {
        for c in v.iter_mut() {
            *c = (f64::from(*c) * factor) as f32;
        }
    }
    for v in &mut mesh.verts_low {
        for c in v.iter_mut() {
            *c = (f64::from(*c) * factor) as f32;
        }
    }
}

/// Where temporary hand-over files for Cura live.
fn handover_dir() -> PathBuf {
    std::env::temp_dir().join("OpenCADStudio-print")
}

// ── App integration ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Print3dMsg {
    ImportPicked(Option<PathBuf>),
    ImportFinished(u64, PathBuf, Result<(Vec<MeshModel>, f64), String>),
    ExportPicked(Option<PathBuf>),
    ExportFinished(PathBuf, Result<(), String>),
    /// 3MF written for Cura; now launch it.
    HandoverReady(PathBuf, Result<(), String>),
    CuraPicked(Option<PathBuf>),
    /// Cura executable picked while a hand-over was waiting for it.
    CuraPickedThenSend(Option<PathBuf>),
    SlicePicked(Option<PathBuf>),
    SliceFinished(PathBuf, Result<SliceReport, String>),
}

/// Transient state: one slice at a time.
#[derive(Debug, Default)]
pub struct Print3dState {
    pub slicing: bool,
}

impl OpenCADStudio {
    fn print3d_meshes(&self, i: usize) -> Vec<MeshModel> {
        self.tabs[i]
            .scene
            .meshes
            .values()
            .filter_map(|s| s.lods.first().cloned())
            .collect()
    }

    fn print3d_unit_mm(&self, i: usize) -> f64 {
        drawing_unit_mm(self.tabs[i].scene.document.header.insertion_units)
    }

    /// The commands of this module; `None` when `cmd` is not one of them.
    pub(in crate::app) fn dispatch_print3d(&mut self, cmd: &str, i: usize) -> Option<Task<Message>> {
        let (verb, rest) = match cmd.split_once(' ') {
            Some((v, r)) => (v, r.trim()),
            None => (cmd, ""),
        };
        match verb {
            "MESHIMPORT" | "IMPORTMESH" | "STLIMPORT" | "IMPORTSTL" | "3MFIMPORT" | "IMPORT3MF" => {
                Some(Task::perform(
                    async {
                        crate::sys::file_dialog()
                            .set_title(crate::t!("Import Mesh").as_ref())
                            .add_filter(crate::t!("Mesh Files").as_ref(), &["stl", "STL", "3mf", "3MF", "obj", "OBJ"])
                            .add_filter(crate::t!("All Files").as_ref(), &["*"])
                            .pick_file()
                            .await
                            .map(|h| crate::sys::handle_path(&h))
                    },
                    |path| Message::Print3d(Print3dMsg::ImportPicked(path)),
                ))
            }
            "3MFOUT" | "EXPORT3MF" => {
                if self.tabs[i].scene.meshes.is_empty() {
                    self.command_line
                        .push_error(crate::t!("3MFOUT: no 3D mesh data in this drawing.").as_ref());
                    return Some(Task::none());
                }
                let name = self.print3d_file_stem(i);
                Some(Task::perform(
                    async move {
                        crate::sys::file_dialog()
                            .set_title(crate::t!("Export 3MF").as_ref())
                            .set_file_name(&format!("{name}.3mf"))
                            .add_filter(crate::t!("3MF Files").as_ref(), &["3mf"])
                            .add_filter(crate::t!("All Files").as_ref(), &["*"])
                            .save_file()
                            .await
                            .map(|h| crate::sys::handle_path(&h))
                    },
                    |path| Message::Print3d(Print3dMsg::ExportPicked(path)),
                ))
            }
            "CURA" | "SENDTOCURA" | "PRINT3D" => Some(self.print3d_send_to_cura(i)),
            "CURAPATH" => Some(Task::perform(
                async {
                    crate::sys::file_dialog()
                        .set_title(crate::t!("Select the Cura executable").as_ref())
                        .pick_file()
                        .await
                        .map(|h| crate::sys::handle_path(&h))
                },
                |path| Message::Print3d(Print3dMsg::CuraPicked(path)),
            )),
            "GCODE" | "SLICE3D" | "CURASLICE" => Some(self.print3d_slice(i)),
            "GCODESETTINGS" => {
                let install = locate(&self.print3d_settings);
                self.command_line.push_output(
                    crate::tf!("GCODESETTINGS: {}", self.print3d_settings.slice.describe()).as_ref(),
                );
                self.command_line.push_info(&format!(
                    "Cura: {}  CuraEngine: {}  definitions: {}",
                    install.cura.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into()),
                    install.engine.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into()),
                    install.definitions.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "-".into()),
                ));
                Some(Task::none())
            }
            "GCODESET" => {
                let mut parts = rest.splitn(2, ' ');
                let name = parts.next().unwrap_or("").trim();
                let value = parts.next().unwrap_or("").trim();
                if name.is_empty() || value.is_empty() {
                    self.command_line.push_error(
                        crate::tf!("GCODESET: usage GCODESET <name> <value>. Settings: {}", SliceSettings::NAMES.join(", ")).as_ref(),
                    );
                    return Some(Task::none());
                }
                match self.print3d_settings.slice.set(name, value) {
                    Ok(()) => {
                        self.command_line.push_output(
                            crate::tf!("GCODESET: {}", self.print3d_settings.slice.describe()).as_ref(),
                        );
                        self.persist_settings_if_changed();
                    }
                    Err(error) => self.command_line.push_error(crate::tf!("GCODESET: {}", error).as_ref()),
                }
                Some(Task::none())
            }
            _ => None,
        }
    }

    fn print3d_file_stem(&self, i: usize) -> String {
        self.tabs[i]
            .current_path
            .as_ref()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "model".into())
    }

    /// Export the meshes as 3MF (millimetres) to `path` on a worker thread.
    fn print3d_export_3mf(&self, i: usize, path: PathBuf, done: impl FnOnce(PathBuf, Result<(), String>) -> Message + Send + 'static) -> Task<Message> {
        let meshes = self.print3d_meshes(i);
        let scale = self.print3d_unit_mm(i);
        let worker_path = path.clone();
        super::update::file::background_task(
            crate::t!("3MF export"),
            move || {
                let refs: Vec<&MeshModel> = meshes.iter().collect();
                let bytes = crate::io::threemf::build_3mf(&refs, scale)?;
                if let Some(parent) = worker_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&worker_path, bytes).map_err(|e| e.to_string())
            },
            move |result| done(path, result),
        )
    }

    fn print3d_send_to_cura(&mut self, i: usize) -> Task<Message> {
        if self.tabs[i].scene.meshes.is_empty() {
            self.command_line
                .push_error(crate::t!("CURA: no 3D mesh data in this drawing.").as_ref());
            return Task::none();
        }
        let install = locate(&self.print3d_settings);
        if install.cura.is_none() {
            self.command_line.push_info(
                crate::t!("CURA: Cura was not found. Select its executable (CURAPATH remembers it).").as_ref(),
            );
            return Task::perform(
                async {
                    crate::sys::file_dialog()
                        .set_title(crate::t!("Select the Cura executable").as_ref())
                        .pick_file()
                        .await
                        .map(|h| crate::sys::handle_path(&h))
                },
                |path| Message::Print3d(Print3dMsg::CuraPickedThenSend(path)),
            );
        }
        let path = handover_dir().join(format!("{}.3mf", self.print3d_file_stem(i)));
        self.print3d_export_3mf(i, path, |path, result| Message::Print3d(Print3dMsg::HandoverReady(path, result)))
    }

    fn print3d_slice(&mut self, i: usize) -> Task<Message> {
        if self.tabs[i].scene.meshes.is_empty() {
            self.command_line
                .push_error(crate::t!("GCODE: no 3D mesh data in this drawing.").as_ref());
            return Task::none();
        }
        if self.print3d.slicing {
            self.command_line.push_error(crate::t!("GCODE: a slice is already running.").as_ref());
            return Task::none();
        }
        let install = locate(&self.print3d_settings);
        if install.engine.is_none() || install.definitions.is_none() {
            self.command_line.push_error(
                crate::t!("GCODE: CuraEngine or its printer definitions were not found. Install UltiMaker Cura and run CURAPATH to point at it.").as_ref(),
            );
            return Task::none();
        }
        let name = self.print3d_file_stem(i);
        Task::perform(
            async move {
                crate::sys::file_dialog()
                    .set_title(crate::t!("Save G-code").as_ref())
                    .set_file_name(&format!("{name}.gcode"))
                    .add_filter(crate::t!("G-code Files").as_ref(), &["gcode"])
                    .add_filter(crate::t!("All Files").as_ref(), &["*"])
                    .save_file()
                    .await
                    .map(|h| crate::sys::handle_path(&h))
            },
            |path| Message::Print3d(Print3dMsg::SlicePicked(path)),
        )
    }

    fn print3d_remember_cura(&mut self, exe: &Path) {
        self.print3d_settings.cura_path = exe.to_string_lossy().into_owned();
        let (engine, definitions) = engine_beside(exe);
        self.print3d_settings.engine_path = engine.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        self.print3d_settings.definitions_dir =
            definitions.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        self.persist_settings_if_changed();
        self.command_line
            .push_output(crate::tf!("CURAPATH: using \"{}\"", exe.display()).as_ref());
        if self.print3d_settings.engine_path.is_empty() || self.print3d_settings.definitions_dir.is_empty() {
            self.command_line.push_info(
                crate::t!("CURAPATH: CuraEngine or its printer definitions were not found next to Cura; GCODE needs both, CURA works without them.").as_ref(),
            );
        }
    }

    pub(in crate::app) fn on_print3d(&mut self, msg: Print3dMsg) -> Task<Message> {
        match msg {
            Print3dMsg::ImportPicked(None)
            | Print3dMsg::ExportPicked(None)
            | Print3dMsg::CuraPicked(None)
            | Print3dMsg::CuraPickedThenSend(None)
            | Print3dMsg::SlicePicked(None) => Task::none(),
            Print3dMsg::ImportPicked(Some(path)) => {
                let tab_id = self.tabs[self.active_tab].id;
                let worker_path = path.clone();
                super::update::file::background_task(
                    crate::t!("Mesh import"),
                    move || read_mesh_file(&worker_path),
                    move |result| Message::Print3d(Print3dMsg::ImportFinished(tab_id, path, result)),
                )
            }
            Print3dMsg::ImportFinished(tab_id, path, result) => {
                let Some(i) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
                    return Task::none();
                };
                match result {
                    Err(error) => self
                        .command_line
                        .push_error(crate::tf!("MESHIMPORT: {}", error).as_ref()),
                    Ok((meshes, file_mm)) => {
                        let stem = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "mesh".into());
                        let factor = file_mm / self.print3d_unit_mm(i);
                        self.push_undo_snapshot(i, "MESHIMPORT");
                        let count = meshes.len();
                        for (index, mut mesh) in meshes.into_iter().enumerate() {
                            scale_mesh(&mut mesh, factor);
                            if mesh.name.is_empty() {
                                mesh.name = if count > 1 { format!("{stem}-{}", index + 1) } else { stem.clone() };
                            }
                            let entity = crate::modules::insert::solid3d_cmds::empty_solid3d();
                            let handle = self.tabs[i].scene.add_entity(entity);
                            if !handle.is_null() {
                                self.tabs[i]
                                    .scene
                                    .meshes
                                    .insert(handle, crate::scene::MeshLodSet::from_single(mesh));
                            }
                        }
                        self.tabs[i].dirty = true;
                        self.command_line.push_output(
                            crate::tf!("MESHIMPORT: imported {} mesh(es) from \"{}\".", count, stem).as_ref(),
                        );
                    }
                }
                Task::none()
            }
            Print3dMsg::ExportPicked(Some(path)) => {
                let i = self.active_tab;
                self.print3d_export_3mf(i, path, |path, result| Message::Print3d(Print3dMsg::ExportFinished(path, result)))
            }
            Print3dMsg::ExportFinished(path, result) => {
                match result {
                    Ok(()) => self.command_line.push_output(
                        crate::tf!("3MFOUT: exported to \"{}\"", path.display()).as_ref(),
                    ),
                    Err(error) => self.command_line.push_error(crate::tf!("3MFOUT: {}", error).as_ref()),
                }
                Task::none()
            }
            Print3dMsg::CuraPicked(Some(exe)) => {
                self.print3d_remember_cura(&exe);
                Task::none()
            }
            Print3dMsg::CuraPickedThenSend(Some(exe)) => {
                self.print3d_remember_cura(&exe);
                let i = self.active_tab;
                self.print3d_send_to_cura(i)
            }
            Print3dMsg::HandoverReady(path, result) => {
                if let Err(error) = result {
                    self.command_line.push_error(crate::tf!("CURA: {}", error).as_ref());
                    return Task::none();
                }
                let install = locate(&self.print3d_settings);
                let Some(cura) = install.cura else {
                    self.command_line.push_error(crate::t!("CURA: Cura was not found. Run CURAPATH to select it.").as_ref());
                    return Task::none();
                };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    match std::process::Command::new(&cura).arg(&path).spawn() {
                        Ok(_) => {
                            log::info!(target: "OpenCADStudio::print3d", "launched {} with {}", cura.display(), path.display());
                            self.command_line.push_output(
                                crate::tf!("CURA: opened \"{}\" in {}", path.display(), cura.display()).as_ref(),
                            );
                        }
                        Err(error) => self.command_line.push_error(crate::tf!("CURA: {}", error).as_ref()),
                    }
                }
                Task::none()
            }
            Print3dMsg::SlicePicked(Some(gcode)) => {
                let i = self.active_tab;
                let install = locate(&self.print3d_settings);
                let (Some(engine), Some(definitions)) = (install.engine, install.definitions) else {
                    self.command_line.push_error(crate::t!("GCODE: CuraEngine or its printer definitions were not found. Install UltiMaker Cura and run CURAPATH to point at it.").as_ref());
                    return Task::none();
                };
                let meshes = self.print3d_meshes(i);
                let scale = self.print3d_unit_mm(i);
                let settings = self.print3d_settings.slice.clone();
                let stl = handover_dir().join(format!("{}-slice.stl", self.print3d_file_stem(i)));
                self.print3d.slicing = true;
                self.command_line.push_info(crate::t!("GCODE: slicing with CuraEngine…").as_ref());
                let out = gcode.clone();
                super::update::file::background_task(
                    crate::t!("G-code slicing"),
                    move || {
                        let mut scaled: Vec<MeshModel> = meshes;
                        for mesh in &mut scaled {
                            scale_mesh(mesh, scale);
                        }
                        let refs: Vec<&MeshModel> = scaled.iter().collect();
                        let bytes = crate::io::stl::build_stl(&refs).ok_or("no mesh data to slice")?;
                        if let Some(parent) = stl.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        std::fs::write(&stl, bytes).map_err(|e| e.to_string())?;
                        let args = engine_args(&definitions, &settings, &stl, &out);
                        log::info!(target: "OpenCADStudio::print3d", "CuraEngine {}", args.join(" "));
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            run_engine(&engine, &args, &out)
                        }
                        #[cfg(target_arch = "wasm32")]
                        {
                            let _ = engine;
                            Err("slicing is desktop only".to_string())
                        }
                    },
                    move |result| Message::Print3d(Print3dMsg::SliceFinished(gcode, result)),
                )
            }
            Print3dMsg::SliceFinished(path, result) => {
                self.print3d.slicing = false;
                match result {
                    Ok(report) => self.command_line.push_output(
                        crate::tf!(
                            "GCODE: wrote \"{}\" — print time {}, filament {}, {} layers",
                            path.display(),
                            report.print_time(),
                            report.filament.clone().unwrap_or_else(|| "?".into()),
                            report.layers.map(|l| l.to_string()).unwrap_or_else(|| "?".into())
                        )
                        .as_ref(),
                    ),
                    Err(error) => self.command_line.push_error(crate::tf!("GCODE: {}", error).as_ref()),
                }
                Task::none()
            }
        }
    }
}

inventory::submit!(crate::command::CommandRegistration {
    names: &[
        "MESHIMPORT", "IMPORTMESH", "STLIMPORT", "IMPORTSTL", "3MFIMPORT", "IMPORT3MF",
        "3MFOUT", "EXPORT3MF", "CURA", "SENDTOCURA", "PRINT3D", "CURAPATH",
        "GCODE", "SLICE3D", "CURASLICE", "GCODESETTINGS", "GCODESET",
    ]
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_settings_parse_describe_and_feed_the_engine() {
        let mut s = SliceSettings::default();
        s.set("layer_height", "0.28").unwrap();
        s.set("infill", "35%").unwrap();
        s.set("support", "on").unwrap();
        s.set("adhesion", "brim").unwrap();
        s.set("walls", "3").unwrap();
        assert!(s.set("infill", "lots").is_err());
        assert!(s.set("nonsense", "1").is_err());
        assert!(s.set("support", "maybe").is_err());
        assert_eq!(s.layer_height, 0.28);
        assert_eq!(s.infill, 35.0);
        assert!(s.support);
        assert_eq!(s.adhesion, Adhesion::Brim);
        let described = s.describe();
        assert!(described.contains("layer_height=0.28") && described.contains("adhesion=brim"));
        let overrides = s.engine_overrides();
        assert!(overrides.windows(2).any(|w| w[0] == "-s" && w[1] == "infill_sparse_density=35"));
        assert!(overrides.windows(2).any(|w| w[0] == "-s" && w[1] == "wall_line_count=3"));
        assert!(overrides.windows(2).any(|w| w[0] == "-s" && w[1] == "support_enable=true"));
        let args = engine_args(Path::new("/defs"), &s, Path::new("/tmp/m.stl"), Path::new("/tmp/m.gcode"));
        assert_eq!(args[0], "slice");
        assert_eq!(args[2], "-j");
        assert!(args[3].ends_with("fdmprinter.def.json"));
        assert_eq!(&args[args.len() - 4..], ["-l", "/tmp/m.stl", "-o", "/tmp/m.gcode"]);
        for name in SliceSettings::NAMES {
            assert!(described.contains(&format!("{name}=")), "{name}");
        }
    }

    #[test]
    fn gcode_header_yields_a_report() {
        let head = ";FLAVOR:Marlin\n;TIME:5432\n;Filament used: 1.234m\n;Layer height: 0.2\n;LAYER_COUNT:120\nG28\n";
        let report = parse_gcode_header(head, 2048);
        assert_eq!(report.print_seconds, Some(5432));
        assert_eq!(report.filament.as_deref(), Some("1.234m"));
        assert_eq!(report.layers, Some(120));
        assert_eq!(report.print_time(), "1h 30m");
        assert_eq!(parse_gcode_header(";TIME:65\n", 1).print_time(), "1m 05s");
        assert_eq!(parse_gcode_header("", 1).print_time(), "?");
    }

    #[test]
    fn engine_is_looked_for_beside_cura_in_each_layout() {
        let root = std::env::temp_dir().join(format!("ocs-cura-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Windows-style flat install.
        let win = root.join("UltiMaker Cura 5.9.0");
        std::fs::create_dir_all(win.join("share/cura/resources/definitions")).unwrap();
        std::fs::write(win.join("UltiMaker-Cura.exe"), b"").unwrap();
        std::fs::write(win.join("CuraEngine.exe"), b"").unwrap();
        std::fs::write(win.join("share/cura/resources/definitions/fdmprinter.def.json"), b"{}").unwrap();
        let (engine, defs) = engine_beside(&win.join("UltiMaker-Cura.exe"));
        assert_eq!(engine, Some(win.join("CuraEngine.exe")));
        assert_eq!(defs, Some(win.join("share/cura/resources/definitions")));
        // macOS bundle.
        let mac = root.join("UltiMaker Cura.app/Contents");
        std::fs::create_dir_all(mac.join("MacOS")).unwrap();
        std::fs::create_dir_all(mac.join("Resources/share/cura/resources/definitions")).unwrap();
        std::fs::write(mac.join("MacOS/UltiMaker-Cura"), b"").unwrap();
        std::fs::write(mac.join("MacOS/CuraEngine"), b"").unwrap();
        std::fs::write(mac.join("Resources/share/cura/resources/definitions/fdmprinter.def.json"), b"{}").unwrap();
        let (engine, defs) = engine_beside(&mac.join("MacOS/UltiMaker-Cura"));
        assert_eq!(engine, Some(mac.join("MacOS/CuraEngine")));
        assert_eq!(defs, Some(mac.join("Resources/share/cura/resources/definitions")));
        // Explicit settings win over detection.
        let settings = Print3dSettings {
            cura_path: win.join("UltiMaker-Cura.exe").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let install = locate(&settings);
        assert_eq!(install.cura, Some(win.join("UltiMaker-Cura.exe")));
        assert_eq!(install.engine, Some(win.join("CuraEngine.exe")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mesh_files_are_read_and_scaled_into_drawing_units() {
        let dir = std::env::temp_dir().join(format!("ocs-mesh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mesh = MeshModel {
            name: "m".into(),
            verts: vec![[0.0, 0.0, 0.0], [25.4, 0.0, 0.0], [0.0, 25.4, 0.0]],
            verts_low: Vec::new(),
            normals: Vec::new(),
            indices: vec![0, 1, 2],
            triangle_material_handles: Vec::new(),
            triangle_colors: Vec::new(),
            color: [1.0; 4],
            selected: false,
        };
        let stl = dir.join("tri.stl");
        std::fs::write(&stl, crate::io::stl::build_stl(&[&mesh]).unwrap()).unwrap();
        let (meshes, mm) = read_mesh_file(&stl).unwrap();
        assert_eq!(meshes.len(), 1);
        assert_eq!(mm, 1.0);
        // Into an inch drawing: 25.4 mm becomes 1 inch.
        let mut imported = meshes.into_iter().next().unwrap();
        scale_mesh(&mut imported, mm / drawing_unit_mm(1));
        assert!((imported.verts[1][0] - 1.0).abs() < 1e-5);
        let three = dir.join("tri.3mf");
        std::fs::write(&three, crate::io::threemf::build_3mf(&[&mesh], 1.0).unwrap()).unwrap();
        let (meshes, mm) = read_mesh_file(&three).unwrap();
        assert_eq!(meshes.len(), 1);
        assert_eq!(mm, 1.0);
        assert!(read_mesh_file(&dir.join("missing.stl")).is_err());
        assert_eq!(drawing_unit_mm(0), 1.0, "unitless drawings count as millimetres");
        assert_eq!(drawing_unit_mm(4), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_are_recognised() {
        let mut app = OpenCADStudio::new();
        assert!(app.dispatch_print3d("LINE", 0).is_none());
        assert!(app.dispatch_print3d("GCODESETTINGS", 0).is_some());
        assert!(app.dispatch_print3d("GCODESET layer_height 0.3", 0).is_some());
        assert_eq!(app.print3d_settings.slice.layer_height, 0.3);
        assert_eq!(app.current_settings().print3d.slice.layer_height, 0.3);
        assert!(app.dispatch_print3d("GCODESET", 0).is_some());
        assert!(app.dispatch_print3d("SLICE", 0).is_none(), "SLICE stays the solid-cutting command");
        // Without meshes the exporters refuse instead of opening a dialog.
        assert!(app.dispatch_print3d("3MFOUT", 0).is_some());
        assert!(app.dispatch_print3d("CURA", 0).is_some());
        assert!(app.dispatch_print3d("GCODE", 0).is_some());
    }
}
