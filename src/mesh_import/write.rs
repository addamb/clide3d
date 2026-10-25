//! Mesh files for printing (`export_print`): binary STL, OBJ, binary
//! little-endian PLY and 3MF (a zip holding the model XML, its entries
//! stored), all in millimeters as the caller scaled them.

use std::io::Write;

use glam::Vec3;

/// Binary STL: face normals from the winding.
pub fn stl(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + tris.len() * 50);
    let mut header = [0u8; 80];
    let label = b"Clide3D export_print";
    header[..label.len()].copy_from_slice(label);
    out.extend(header);
    out.extend((tris.len() as u32).to_le_bytes());
    for t in tris {
        let [a, b, c] = t.map(|i| points[i as usize]);
        let n = (b - a).cross(c - a).normalize_or_zero();
        for v in [n, a, b, c] {
            for x in v.to_array() {
                out.extend(x.to_le_bytes());
            }
        }
        out.extend([0u8; 2]);
    }
    out
}

pub fn obj(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 40 + tris.len() * 24);
    let _ = writeln!(out, "# Clide3D export_print: {} vertices, {} triangles, millimeters", points.len(), tris.len());
    for p in points {
        let _ = writeln!(out, "v {} {} {}", p.x, p.y, p.z);
    }
    for t in tris {
        let _ = writeln!(out, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    out
}

pub fn ply(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 12 + tris.len() * 13 + 256);
    let _ = write!(out, "ply\nformat binary_little_endian 1.0\ncomment Clide3D export_print, millimeters\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\nelement face {}\nproperty list uchar uint vertex_indices\nend_header\n", points.len(), tris.len());
    for p in points {
        for x in p.to_array() {
            out.extend(x.to_le_bytes());
        }
    }
    for t in tris {
        out.push(3);
        for i in t {
            out.extend(i.to_le_bytes());
        }
    }
    out
}

/// 3MF: one object of one mesh, units millimeters.
pub fn three_mf(points: &[Vec3], tris: &[[u32; 3]]) -> Result<Vec<u8>, String> {
    let mut model = String::with_capacity(points.len() * 48 + tris.len() * 40 + 512);
    model.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n <resources>\n  <object id=\"1\" type=\"model\">\n   <mesh>\n    <vertices>\n");
    for p in points {
        model.push_str(&format!("     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n", p.x, p.y, p.z));
    }
    model.push_str("    </vertices>\n    <triangles>\n");
    for t in tris {
        model.push_str(&format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0], t[1], t[2]));
    }
    model.push_str("    </triangles>\n   </mesh>\n  </object>\n </resources>\n <build>\n  <item objectid=\"1\"/>\n </build>\n</model>\n");
    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/></Types>\n";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/></Relationships>\n";
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(model.len() > u32::MAX as usize / 2);
    for (name, body) in [("[Content_Types].xml", types.as_bytes()), ("_rels/.rels", rels.as_bytes()), ("3D/3dmodel.model", model.as_bytes())] {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(body).map_err(|e| e.to_string())?;
    }
    Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
}
