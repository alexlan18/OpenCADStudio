// STL binary export — converts all tessellated MeshModels in the scene to a
// single binary STL file.
//
// Binary STL format:
//   80-byte header
//   4-byte triangle count (u32 LE)
//   Per triangle (50 bytes):
//     3 × f32 normal
//     3 × 3 × f32 vertices
//     2-byte attribute (0)

use std::io::Write;

use crate::scene::model::mesh_model::MeshModel;

/// Build a binary STL byte buffer from a slice of mesh models.
/// Returns `None` if there are no triangles to export.
pub fn build_stl(meshes: &[&MeshModel]) -> Option<Vec<u8>> {
    // Collect all triangles.
    struct Tri {
        normal: [f32; 3],
        v: [[f32; 3]; 3],
    }

    let mut tris: Vec<Tri> = Vec::new();

    for mesh in meshes {
        let verts = &mesh.verts;
        let idx = &mesh.indices;
        let n_tri = idx.len() / 3;
        for t in 0..n_tri {
            let i0 = idx[t * 3] as usize;
            let i1 = idx[t * 3 + 1] as usize;
            let i2 = idx[t * 3 + 2] as usize;
            if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() {
                continue;
            }
            let a = verts[i0];
            let b = verts[i1];
            let c = verts[i2];

            // The facet normal is the triangle's own. A smoothed vertex normal
            // leans off it, so only a triangle whose cross product is zero
            // falls back to the normal the mesh gives it. Small facets have a
            // tiny cross product but still a normal of their own.
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let nx = ab[1] * ac[2] - ab[2] * ac[1];
            let ny = ab[2] * ac[0] - ab[0] * ac[2];
            let nz = ab[0] * ac[1] - ab[1] * ac[0];
            let len = nx.hypot(ny).hypot(nz);
            let normal = if len == 0.0 && i0 < mesh.normals.len() {
                mesh.normals[i0]
            } else {
                let len = len.max(f32::MIN_POSITIVE);
                [nx / len, ny / len, nz / len]
            };

            tris.push(Tri {
                normal,
                v: [a, b, c],
            });
        }
    }

    if tris.is_empty() {
        return None;
    }

    let mut buf: Vec<u8> = Vec::with_capacity(84 + tris.len() * 50);

    // 80-byte header.
    let mut header = [0u8; 80];
    let title = b"Open CAD Studio STL export";
    header[..title.len()].copy_from_slice(title);
    buf.extend_from_slice(&header);

    // Triangle count.
    buf.extend_from_slice(&(tris.len() as u32).to_le_bytes());

    for tri in &tris {
        // Normal.
        for &f in &tri.normal {
            buf.write_all(&f.to_le_bytes()).ok()?;
        }
        // Vertices.
        for v in &tri.v {
            for &f in v {
                buf.write_all(&f.to_le_bytes()).ok()?;
            }
        }
        // Attribute byte count = 0.
        buf.extend_from_slice(&0u16.to_le_bytes());
    }

    Some(buf)
}


/// Parse an STL file (binary or ASCII) into one mesh with flat per-face
/// normals. Returns `None` when no triangle could be read.
pub fn parse_stl(bytes: &[u8], color: [f32; 4]) -> Option<MeshModel> {
    let tris = if looks_ascii(bytes) {
        parse_ascii(bytes)
    } else {
        parse_binary(bytes)
    };
    if tris.is_empty() {
        return None;
    }
    let mut verts = Vec::with_capacity(tris.len() * 3);
    let mut normals = Vec::with_capacity(tris.len() * 3);
    let mut indices = Vec::with_capacity(tris.len() * 3);
    for tri in tris {
        let n = face_normal(tri);
        for v in tri {
            indices.push(verts.len() as u32);
            verts.push(v);
            normals.push(n);
        }
    }
    Some(MeshModel {
        name: String::new(),
        verts,
        verts_low: Vec::new(),
        normals,
        indices,
        triangle_material_handles: Vec::new(),
        triangle_colors: Vec::new(),
        color,
        selected: false,
    })
}

fn face_normal(tri: [[f32; 3]; 3]) -> [f32; 3] {
    let [a, b, c] = tri;
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// A binary STL of N triangles is exactly 84 + 50 N bytes; anything else
/// that starts with `solid` is ASCII.
fn looks_ascii(bytes: &[u8]) -> bool {
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if bytes.len() == 84 + count * 50 {
            return false;
        }
    }
    let start = bytes.iter().take_while(|b| b.is_ascii_whitespace()).count();
    bytes[start..].starts_with(b"solid")
}

fn parse_binary(bytes: &[u8]) -> Vec<[[f32; 3]; 3]> {
    if bytes.len() < 84 {
        return Vec::new();
    }
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let available = (bytes.len() - 84) / 50;
    let f = |at: usize| f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (0..count.min(available))
        .map(|t| {
            let base = 84 + t * 50 + 12; // skip the normal
            let v = |k: usize| [f(base + k * 12), f(base + k * 12 + 4), f(base + k * 12 + 8)];
            [v(0), v(1), v(2)]
        })
        .filter(|tri| tri.iter().all(|v| v.iter().all(|c| c.is_finite())))
        .collect()
}

fn parse_ascii(bytes: &[u8]) -> Vec<[[f32; 3]; 3]> {
    let text = String::from_utf8_lossy(bytes);
    let mut tris = Vec::new();
    let mut current: Vec<[f32; 3]> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("vertex") => {
                let mut v = [0f32; 3];
                let mut ok = true;
                for c in &mut v {
                    *c = match words.next().and_then(|w| w.parse::<f32>().ok()) {
                        Some(value) if value.is_finite() => value,
                        _ => {
                            ok = false;
                            break;
                        }
                    };
                }
                if ok {
                    current.push(v);
                }
            }
            Some("endfacet") => {
                if current.len() == 3 {
                    tris.push([current[0], current[1], current[2]]);
                }
                current.clear();
            }
            _ => {}
        }
    }
    tris
}

#[cfg(test)]
mod tests {
    use super::build_stl;
    use crate::scene::model::mesh_model::MeshModel;

    fn triangle(verts: Vec<[f32; 3]>, normals: Vec<[f32; 3]>) -> MeshModel {
        MeshModel {
            name: String::new(),
            verts,
            verts_low: Vec::new(),
            normals,
            indices: vec![0, 1, 2],
            triangle_material_handles: Vec::new(),
            triangle_colors: Vec::new(),
            color: [1.0; 4],
            selected: false,
        }
    }

    /// The first facet's normal: the 12 bytes after the 80-byte header and
    /// the 4-byte triangle count.
    fn first_facet_normal(stl: &[u8]) -> [f32; 3] {
        [0, 1, 2].map(|k| {
            let at = 84 + 4 * k;
            f32::from_le_bytes(stl[at..at + 4].try_into().expect("4 bytes"))
        })
    }

    /// A curved surface shares smoothed normals across its facets, so a
    /// vertex normal leans away from the triangle. STL stores the facet's
    /// own normal.
    #[test]
    fn the_facet_normal_is_the_triangles_own() {
        let leaning = [
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let mesh = triangle(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![leaning; 3],
        );
        let stl = build_stl(&[&mesh]).expect("stl");
        assert_eq!(first_facet_normal(&stl), [0.0, 0.0, 1.0]);
    }

    /// A small facet still has a normal of its own. Its cross product is tiny
    /// (1e-8 here), far below f32::EPSILON, but it is not zero.
    #[test]
    fn a_small_facet_keeps_its_own_normal() {
        let leaning = [
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let mesh = triangle(
            vec![[0.0, 0.0, 0.0], [1e-4, 0.0, 0.0], [0.0, 1e-4, 0.0]],
            vec![leaning; 3],
        );
        let stl = build_stl(&[&mesh]).expect("stl");
        assert_eq!(first_facet_normal(&stl), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_facet_whose_squared_cross_product_underflows_keeps_its_own_normal() {
        let mesh = triangle(
            vec![[0.0, 0.0, 0.0], [1.0e-12, 0.0, 0.0], [0.0, 1.0e-12, 0.0]],
            vec![[0.0, 1.0, 0.0]; 3],
        );
        let stl = build_stl(&[&mesh]).expect("stl");
        assert_eq!(first_facet_normal(&stl), [0.0, 0.0, 1.0]);
    }

    /// A facet with no area has no normal of its own, so it keeps the one
    /// the mesh gives it rather than a zero vector.
    #[test]
    fn a_degenerate_facet_keeps_its_vertex_normal() {
        let mesh = triangle(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
            vec![[0.0, 1.0, 0.0]; 3],
        );
        let stl = build_stl(&[&mesh]).expect("stl");
        assert_eq!(first_facet_normal(&stl), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn stl_round_trips_through_binary_and_reads_ascii() {
        let mesh = MeshModel {
            name: String::new(),
            verts: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            verts_low: Vec::new(),
            normals: Vec::new(),
            indices: vec![0, 1, 2, 0, 1, 3],
            triangle_material_handles: Vec::new(),
            triangle_colors: Vec::new(),
            color: [1.0; 4],
            selected: false,
        };
        let bytes = build_stl(&[&mesh]).expect("stl");
        let parsed = super::parse_stl(&bytes, [0.5; 4]).expect("parse binary");
        assert_eq!(parsed.indices.len(), 6);
        assert_eq!(parsed.verts.len(), 6);
        assert_eq!(parsed.verts[1], [10.0, 0.0, 0.0]);
        assert_eq!(parsed.normals[0], [0.0, 0.0, 1.0]);
        let ascii = b"solid tri\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid tri\n";
        let parsed = super::parse_stl(ascii, [0.5; 4]).expect("parse ascii");
        assert_eq!(parsed.indices, vec![0, 1, 2]);
        assert_eq!(parsed.verts[2], [0.0, 1.0, 0.0]);
        assert!(super::parse_stl(b"solid empty\nendsolid empty\n", [0.5; 4]).is_none());
        assert!(super::parse_stl(&[0u8; 10], [0.5; 4]).is_none());
    }
}
