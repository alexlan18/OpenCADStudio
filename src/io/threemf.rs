//! 3MF (3D Manufacturing Format) read and write — the container slicers
//! such as Cura prefer: a zip holding one XML model with explicit units.
//!
//! Writing produces the minimal core-spec package: `[Content_Types].xml`,
//! `_rels/.rels` and `3D/3dmodel.model` with one `object` per mesh and a
//! `build` placing each at the origin. Reading handles meshes, components
//! (object instances with a transform) and build-item transforms, and reports
//! the file's unit so the caller can scale into drawing units.

use crate::scene::model::mesh_model::MeshModel;
use std::io::{Cursor, Read, Write};

const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";

/// Millimetres per 3MF unit name.
pub fn unit_to_mm(unit: &str) -> f64 {
    match unit {
        "micron" => 0.001,
        "centimeter" => 10.0,
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        _ => 1.0, // millimeter (the default)
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The `3D/3dmodel.model` document for `meshes`, every coordinate multiplied
/// by `scale` (drawing units → millimetres).
pub fn build_model_xml(meshes: &[&MeshModel], scale: f64) -> String {
    let mut xml = String::with_capacity(4096);
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"{CORE_NS}\">\n <resources>\n"
    ));
    let mut ids = Vec::new();
    for (index, mesh) in meshes.iter().enumerate() {
        if mesh.indices.len() < 3 || mesh.verts.is_empty() {
            continue;
        }
        let id = index + 1;
        ids.push(id);
        let name = if mesh.name.is_empty() {
            format!("mesh{id}")
        } else {
            mesh.name.clone()
        };
        xml.push_str(&format!(
            "  <object id=\"{id}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>\n",
            xml_escape(&name)
        ));
        for (i, v) in mesh.verts.iter().enumerate() {
            let low = mesh.verts_low.get(i).copied().unwrap_or([0.0; 3]);
            let x = (v[0] as f64 + low[0] as f64) * scale;
            let y = (v[1] as f64 + low[1] as f64) * scale;
            let z = (v[2] as f64 + low[2] as f64) * scale;
            xml.push_str(&format!("     <vertex x=\"{x}\" y=\"{y}\" z=\"{z}\"/>\n"));
        }
        xml.push_str("    </vertices>\n    <triangles>\n");
        let n = mesh.verts.len() as u32;
        for tri in mesh.indices.chunks_exact(3) {
            if tri.iter().all(|i| *i < n) {
                xml.push_str(&format!(
                    "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n",
                    tri[0], tri[1], tri[2]
                ));
            }
        }
        xml.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    xml.push_str(" </resources>\n <build>\n");
    for id in ids {
        xml.push_str(&format!("  <item objectid=\"{id}\"/>\n"));
    }
    xml.push_str(" </build>\n</model>\n");
    xml
}

/// A complete `.3mf` package. `scale` converts drawing units to millimetres.
pub fn build_3mf(meshes: &[&MeshModel], scale: f64) -> Result<Vec<u8>, String> {
    if !meshes.iter().any(|m| m.indices.len() >= 3) {
        return Err("no mesh data to export".into());
    }
    let model = build_model_xml(meshes, scale);
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n</Types>\n";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n</Relationships>\n";
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("3D/3dmodel.model", model.as_str()),
    ] {
        writer
            .start_file(name, options)
            .and_then(|_| writer.write_all(body.as_bytes()).map_err(Into::into))
            .map_err(|e| format!("3MF: cannot write {name}: {e}"))?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|e| format!("3MF: {e}"))
}

/// What a 3MF file contained.
pub struct ThreeMf {
    pub meshes: Vec<MeshModel>,
    /// The file's unit name (`millimeter` when unspecified).
    pub unit: String,
}

/// Read a `.3mf` package. `color` is applied to every mesh; coordinates stay
/// in the file's unit (see [`ThreeMf::unit`]).
pub fn parse_3mf(bytes: &[u8], color: [f32; 4]) -> Result<ThreeMf, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("3MF: {e}"))?;
    let model_name = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .find(|name| name.eq_ignore_ascii_case("3D/3dmodel.model"))
        .or_else(|| {
            (0..archive.len())
                .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
                .find(|name| name.to_ascii_lowercase().ends_with(".model"))
        })
        .ok_or("3MF: no 3D model part in the package")?;
    let mut xml = String::new();
    archive
        .by_name(&model_name)
        .map_err(|e| format!("3MF: {e}"))?
        .read_to_string(&mut xml)
        .map_err(|e| format!("3MF: {e}"))?;
    parse_model_xml(&xml, color)
}

type Transform = [f64; 12];
const IDENTITY: Transform = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn parse_transform(text: Option<&str>) -> Transform {
    let Some(text) = text else { return IDENTITY };
    let values: Vec<f64> = text
        .split_whitespace()
        .filter_map(|v| v.parse::<f64>().ok())
        .collect();
    if values.len() == 12 && values.iter().all(|v| v.is_finite()) {
        let mut t = IDENTITY;
        t.copy_from_slice(&values);
        t
    } else {
        IDENTITY
    }
}

/// 3MF transforms are row vectors: `p' = p · M` with M the 4×3 matrix given
/// as `m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32`.
fn apply(t: &Transform, p: [f64; 3]) -> [f64; 3] {
    [
        p[0] * t[0] + p[1] * t[3] + p[2] * t[6] + t[9],
        p[0] * t[1] + p[1] * t[4] + p[2] * t[7] + t[10],
        p[0] * t[2] + p[1] * t[5] + p[2] * t[8] + t[11],
    ]
}

fn compose(outer: &Transform, inner: &Transform) -> Transform {
    // p · inner · outer
    let mut out = IDENTITY;
    for row in 0..4 {
        let p = [inner[row * 3], inner[row * 3 + 1], inner[row * 3 + 2]];
        let q = if row == 3 {
            apply(outer, p)
        } else {
            [
                p[0] * outer[0] + p[1] * outer[3] + p[2] * outer[6],
                p[0] * outer[1] + p[1] * outer[4] + p[2] * outer[7],
                p[0] * outer[2] + p[1] * outer[5] + p[2] * outer[8],
            ]
        };
        out[row * 3..row * 3 + 3].copy_from_slice(&q);
    }
    out
}

struct RawMesh {
    name: String,
    verts: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
}

struct RawObject {
    mesh: Option<RawMesh>,
    components: Vec<(String, Transform)>,
}

pub fn parse_model_xml(xml: &str, color: [f32; 4]) -> Result<ThreeMf, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("3MF: invalid model XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "model" {
        return Err("3MF: root element is not <model>".into());
    }
    let unit = root.attribute("unit").unwrap_or("millimeter").to_string();
    let mut objects: std::collections::HashMap<String, RawObject> = Default::default();
    let find = |node: roxmltree::Node, name: &str| node.children().find(|c| c.is_element() && c.tag_name().name() == name);
    if let Some(resources) = find(root, "resources") {
        for object in resources.children().filter(|c| c.is_element() && c.tag_name().name() == "object") {
            let Some(id) = object.attribute("id") else { continue };
            let name = object.attribute("name").unwrap_or("").to_string();
            let mut raw = RawObject { mesh: None, components: Vec::new() };
            if let Some(mesh) = find(object, "mesh") {
                let mut verts = Vec::new();
                if let Some(vertices) = find(mesh, "vertices") {
                    for v in vertices.children().filter(|c| c.is_element() && c.tag_name().name() == "vertex") {
                        let read = |k: &str| v.attribute(k).and_then(|t| t.parse::<f64>().ok()).filter(|f| f.is_finite());
                        if let (Some(x), Some(y), Some(z)) = (read("x"), read("y"), read("z")) {
                            verts.push([x, y, z]);
                        } else {
                            verts.push([0.0, 0.0, 0.0]);
                        }
                    }
                }
                let mut tris = Vec::new();
                if let Some(triangles) = find(mesh, "triangles") {
                    for t in triangles.children().filter(|c| c.is_element() && c.tag_name().name() == "triangle") {
                        let read = |k: &str| t.attribute(k).and_then(|v| v.parse::<u32>().ok());
                        if let (Some(a), Some(b), Some(c)) = (read("v1"), read("v2"), read("v3")) {
                            let n = verts.len() as u32;
                            if a < n && b < n && c < n {
                                tris.push([a, b, c]);
                            }
                        }
                    }
                }
                raw.mesh = Some(RawMesh { name, verts, tris });
            }
            if let Some(components) = find(object, "components") {
                for c in components.children().filter(|c| c.is_element() && c.tag_name().name() == "component") {
                    if let Some(target) = c.attribute("objectid") {
                        raw.components.push((target.to_string(), parse_transform(c.attribute("transform"))));
                    }
                }
            }
            objects.insert(id.to_string(), raw);
        }
    }

    let mut meshes = Vec::new();
    fn emit(
        objects: &std::collections::HashMap<String, RawObject>,
        id: &str,
        transform: &Transform,
        depth: usize,
        color: [f32; 4],
        out: &mut Vec<MeshModel>,
    ) {
        if depth > 16 {
            return;
        }
        let Some(object) = objects.get(id) else { return };
        if let Some(raw) = &object.mesh {
            if !raw.tris.is_empty() {
                let verts: Vec<[f32; 3]> = raw
                    .verts
                    .iter()
                    .map(|v| {
                        let p = apply(transform, *v);
                        [p[0] as f32, p[1] as f32, p[2] as f32]
                    })
                    .collect();
                let indices: Vec<u32> = raw.tris.iter().flat_map(|t| t.iter().copied()).collect();
                out.push(MeshModel {
                    name: raw.name.clone(),
                    verts,
                    verts_low: Vec::new(),
                    normals: Vec::new(),
                    indices,
                    triangle_material_handles: Vec::new(),
                    triangle_colors: Vec::new(),
                    color,
                    selected: false,
                });
            }
        }
        for (child, local) in &object.components {
            emit(objects, child, &compose(transform, local), depth + 1, color, out);
        }
    }
    if let Some(build) = find(root, "build") {
        for item in build.children().filter(|c| c.is_element() && c.tag_name().name() == "item") {
            if let Some(id) = item.attribute("objectid") {
                emit(&objects, id, &parse_transform(item.attribute("transform")), 0, color, &mut meshes);
            }
        }
    }
    if meshes.is_empty() {
        // A file without build items: take every mesh object as is.
        let mut ids: Vec<&String> = objects.keys().collect();
        ids.sort();
        for id in ids {
            emit(&objects, id, &IDENTITY, 0, color, &mut meshes);
        }
    }
    if meshes.is_empty() {
        return Err("3MF: no mesh geometry in the model".into());
    }
    Ok(ThreeMf { meshes, unit })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_like() -> MeshModel {
        MeshModel {
            name: "part".into(),
            verts: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            verts_low: Vec::new(),
            normals: Vec::new(),
            indices: vec![0, 1, 2, 0, 1, 3, 0, 2, 3, 1, 2, 3],
            triangle_material_handles: Vec::new(),
            triangle_colors: Vec::new(),
            color: [1.0; 4],
            selected: false,
        }
    }

    #[test]
    fn three_mf_round_trips_with_unit_scaling() {
        let mesh = cube_like();
        let bytes = build_3mf(&[&mesh], 25.4).expect("3mf");
        let parsed = parse_3mf(&bytes, [0.5; 4]).expect("parse");
        assert_eq!(parsed.unit, "millimeter");
        assert_eq!(parsed.meshes.len(), 1);
        assert_eq!(parsed.meshes[0].name, "part");
        assert_eq!(parsed.meshes[0].indices.len(), 12);
        assert!((parsed.meshes[0].verts[1][0] - 254.0).abs() < 1e-3);
        assert!(build_3mf(&[], 1.0).is_err());
        assert_eq!(unit_to_mm("inch"), 25.4);
        assert_eq!(unit_to_mm("millimeter"), 1.0);
    }

    #[test]
    fn components_and_build_transforms_are_applied() {
        let xml = r#"<?xml version="1.0"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model"><mesh>
   <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
   <triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
  <object id="2" type="model"><components><component objectid="1" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></components></object>
 </resources>
 <build><item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 7 0"/></build>
</model>"#;
        let parsed = parse_model_xml(xml, [0.5; 4]).expect("parse");
        assert_eq!(parsed.unit, "centimeter");
        assert_eq!(parsed.meshes.len(), 1);
        // Component offset (5,0,0) then build offset (0,7,0).
        assert_eq!(parsed.meshes[0].verts[0], [5.0, 7.0, 0.0]);
        assert_eq!(parsed.meshes[0].verts[1], [6.0, 7.0, 0.0]);
        assert!(parse_model_xml("<model/>", [0.5; 4]).is_err());
        assert!(parse_model_xml("not xml", [0.5; 4]).is_err());
    }
}
