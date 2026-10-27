//! USD as text (`.usda`; D18's optional writer, in-house: USDA is text):
//! a `UsdGeomMesh` with Catmull-Clark subdivision, edge-and-corner
//! borders, linear face-varying UVs and semi-sharp creases, under an
//! `Xform` at the object's world placement, bound to a
//! `UsdPreviewSurface` whose textures are the maps beside the file (the
//! scalar displacement map on its `displacement` input, decoded by the
//! texture's scale and bias). `read` is the in-house sanity check: the
//! file's brackets balance, the mesh has its required attributes, the
//! counts agree, and it gives the cage back.

use std::fmt::Write as _;

use glam::{Mat4, Vec3};

use super::Cage;

/// The first line of every USDA file.
pub const HEADER: &str = "#usda 1.0";

/// Deepest bracket nesting `read` accepts.
const MAX_DEPTH: usize = 64;

/// A texture the material reads: its file, the shader input it feeds and
/// how its texels decode (value = texel * scale + bias, per channel).
pub struct Texture {
    pub file: String,
    pub input: &'static str,
    pub output: &'static str,
    pub kind: &'static str,
    pub raw: bool,
    pub scale: [f32; 4],
    pub bias: [f32; 4],
}

/// A USD identifier from a name (letters, digits and _; not starting
/// with a digit).
fn ident(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(64).collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

fn floats(v: &[f32]) -> String {
    v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(", ")
}

/// The cage as USDA text. `world` places it (USD's row-vector matrices:
/// each of glam's columns is a row); `textures` are the maps the
/// material reads; `notes` go in the layer's doc string.
pub fn write(cage: &Cage, name: &str, world: Mat4, textures: &[Texture], notes: &str) -> Vec<u8> {
    let root = ident(name);
    let mut o = String::with_capacity(cage.points.len() * 48 + cage.corners.len() * 40 + 2048);
    let _ = writeln!(o, "{HEADER}");
    let _ = writeln!(o, "(\n    defaultPrim = \"{root}\"\n    metersPerUnit = 1\n    upAxis = \"Y\"\n    doc = \"Clide3D export_poly. {}\"\n)\n", notes.chars().map(|c| if c.is_control() || ['"', '\\', '[', ']', '(', ')', '{', '}', '@'].contains(&c) { ' ' } else { c }).collect::<String>());
    let _ = writeln!(o, "def Xform \"{root}\"\n{{");
    let rows: Vec<String> = world.to_cols_array_2d().iter().map(|c| format!("({})", c.iter().map(|x| format!("{}", *x as f64)).collect::<Vec<_>>().join(", "))).collect();
    let _ = writeln!(o, "    matrix4d xformOp:transform = ( {} )", rows.join(", "));
    o.push_str("    uniform token[] xformOpOrder = [\"xformOp:transform\"]\n\n");
    o.push_str("    def Mesh \"cage\"\n    {\n");
    o.push_str("        uniform token subdivisionScheme = \"catmullClark\"\n");
    o.push_str("        uniform token interpolateBoundary = \"edgeAndCorner\"\n");
    o.push_str("        uniform token faceVaryingLinearInterpolation = \"all\"\n");
    o.push_str("        uniform token orientation = \"rightHanded\"\n");
    let pts: Vec<String> = cage.points.iter().map(|p| format!("({}, {}, {})", p.x, p.y, p.z)).collect();
    let _ = writeln!(o, "        point3f[] points = [{}]", pts.join(", "));
    let counts: Vec<String> = cage.face_start.windows(2).map(|w| (w[1] - w[0]).to_string()).collect();
    let _ = writeln!(o, "        int[] faceVertexCounts = [{}]", counts.join(", "));
    let _ = writeln!(o, "        int[] faceVertexIndices = [{}]", cage.corners.iter().map(u32::to_string).collect::<Vec<_>>().join(", "));
    if !cage.creases.is_empty() {
        let _ = writeln!(o, "        int[] creaseIndices = [{}]", cage.creases.iter().map(|c| format!("{}, {}", c.0, c.1)).collect::<Vec<_>>().join(", "));
        let _ = writeln!(o, "        int[] creaseLengths = [{}]", vec!["2"; cage.creases.len()].join(", "));
        let _ = writeln!(o, "        float[] creaseSharpnesses = [{}]", floats(&cage.creases.iter().map(|c| c.2).collect::<Vec<_>>()));
    }
    if let Some(uv) = &cage.uv {
        // USD's st has v up: 1 - v of the top-left UVs, in f64 (exact);
        // UDIM layouts flip tile by tile (`uv::v_out`).
        let udim = crate::poly::uv::spans_tiles(uv);
        let st: Vec<String> = uv.iter().map(|t| format!("({}, {})", t[0], crate::poly::uv::v_out(t[1], udim))).collect();
        let mark = if udim { format!("            doc = \"{}\"\n", crate::poly::uv::UDIM_MARK) } else { String::new() };
        let _ = writeln!(o, "        texCoord2f[] primvars:st = [{}] (\n{mark}            interpolation = \"faceVarying\"\n        )", st.join(", "));
    }
    if !textures.is_empty() {
        let _ = writeln!(o, "        rel material:binding = </{root}/Looks/Material>");
    }
    o.push_str("    }\n");
    if !textures.is_empty() {
        let m = format!("/{root}/Looks/Material");
        let _ = writeln!(o, "\n    def Scope \"Looks\"\n    {{\n        def Material \"Material\"\n        {{");
        let _ = writeln!(o, "            token outputs:surface.connect = <{m}/Surface.outputs:surface>");
        if textures.iter().any(|t| t.input == "displacement") {
            let _ = writeln!(o, "            token outputs:displacement.connect = <{m}/Surface.outputs:displacement>");
        }
        let _ = writeln!(o, "\n            def Shader \"Surface\"\n            {{\n                uniform token info:id = \"UsdPreviewSurface\"");
        for t in textures {
            let shader = ident(&format!("{}_texture", t.input));
            let _ = writeln!(o, "                {} inputs:{}.connect = <{m}/{shader}.outputs:{}>", t.kind, t.input, t.output);
        }
        o.push_str("                token outputs:surface\n");
        if textures.iter().any(|t| t.input == "displacement") {
            o.push_str("                token outputs:displacement\n");
        }
        o.push_str("            }\n");
        let _ = writeln!(o, "\n            def Shader \"st_reader\"\n            {{\n                uniform token info:id = \"UsdPrimvarReader_float2\"\n                string inputs:varname = \"st\"\n                float2 outputs:result\n            }}");
        for t in textures {
            let shader = ident(&format!("{}_texture", t.input));
            let _ = writeln!(o, "\n            def Shader \"{shader}\"\n            {{\n                uniform token info:id = \"UsdUVTexture\"\n                asset inputs:file = @{}@", t.file.replace('@', "_"));
            let _ = writeln!(o, "                float2 inputs:st.connect = <{m}/st_reader.outputs:result>");
            let _ = writeln!(o, "                token inputs:sourceColorSpace = \"{}\"", if t.raw { "raw" } else { "sRGB" });
            let _ = writeln!(o, "                float4 inputs:scale = ({})\n                float4 inputs:bias = ({})", floats(&t.scale), floats(&t.bias));
            let _ = writeln!(o, "                {} outputs:{}\n            }}", if t.output == "rgb" { "float3" } else { "float" }, t.output);
        }
        o.push_str("        }\n    }\n");
    }
    o.push_str("}\n");
    o.into_bytes()
}

/// The text between `name = [` and its `]` on the mesh's attribute
/// `name` (None without it).
fn array<'a>(text: &'a str, decl: &str) -> Option<&'a str> {
    let at = text.find(&format!("{decl} = ["))? + decl.len() + 4;
    let end = text[at..].find(']')?;
    Some(&text[at..at + end])
}

/// Numbers of an array's text (brackets, parentheses and commas skipped),
/// at most `max` of them.
fn numbers<T: std::str::FromStr>(s: &str, max: usize, what: &str) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for w in s.split(|c: char| c == ',' || c == '(' || c == ')' || c.is_whitespace()).filter(|w| !w.is_empty()) {
        if out.len() >= max {
            return Err(format!("{what} has more than {max} values"));
        }
        out.push(w.parse().map_err(|_| format!("{what}: {w} is not a number"))?);
    }
    Ok(out)
}

/// Checks a USDA file's shape and reads its cage back: the header;
/// braces, brackets and parentheses balanced outside strings and asset
/// paths; a Catmull-Clark mesh with points, faceVertexCounts and
/// faceVertexIndices whose counts agree (indices within the points, the
/// crease lists consistent, a faceVarying st per corner). Counts are
/// bounded by the Poly caps as the numbers are read.
pub fn read(bytes: &[u8]) -> Result<Cage, String> {
    read_as(bytes, None)
}

/// `read`, its UVs a UDIM layout when `udim` says so (None: when the file
/// is marked, `uv::UDIM_MARK`).
pub fn read_as(bytes: &[u8], udim: Option<bool>) -> Result<Cage, String> {
    use crate::poly::{MAX_CAGE_CORNERS, MAX_CAGE_FACES, MAX_CAGE_POINTS, MAX_CREASES};
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8")?;
    if !text.starts_with(HEADER) {
        return Err(format!("the file does not start with {HEADER}"));
    }
    // Open brackets, as bytes; nesting is bounded (this writer nests 6
    // deep), so a file of brackets cannot grow it.
    let mut depth: Vec<u8> = Vec::new();
    let (mut in_string, mut in_asset) = (false, false);
    for (i, c) in text.char_indices() {
        match c {
            '"' if !in_asset => in_string = !in_string,
            '@' if !in_string => in_asset = !in_asset,
            _ if in_string || in_asset => {}
            '{' | '[' | '(' => {
                if depth.len() >= MAX_DEPTH {
                    return Err(format!("brackets nest more than {MAX_DEPTH} deep at byte {i}"));
                }
                depth.push(c as u8);
            }
            '}' | ']' | ')' => {
                let want = match c {
                    '}' => b'{',
                    ']' => b'[',
                    _ => b'(',
                };
                if depth.pop() != Some(want) {
                    return Err(format!("an unbalanced {c} at byte {i}"));
                }
            }
            _ => {}
        }
    }
    if !depth.is_empty() || in_string || in_asset {
        return Err("brackets, quotes or asset paths are left open".into());
    }
    for required in ["def Mesh", "uniform token subdivisionScheme = \"catmullClark\"", "point3f[] points", "int[] faceVertexCounts", "int[] faceVertexIndices"] {
        if !text.contains(required) {
            return Err(format!("the mesh has no {required}"));
        }
    }
    let flat: Vec<f32> = numbers(array(text, "point3f[] points").ok_or("points is not an array")?, 3 * MAX_CAGE_POINTS, "points")?;
    if flat.len() % 3 != 0 {
        return Err("points are not triples".into());
    }
    let points: Vec<Vec3> = flat.chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
    let counts: Vec<u32> = numbers(array(text, "int[] faceVertexCounts").ok_or("faceVertexCounts is not an array")?, MAX_CAGE_FACES, "faceVertexCounts")?;
    let corners: Vec<u32> = numbers(array(text, "int[] faceVertexIndices").ok_or("faceVertexIndices is not an array")?, MAX_CAGE_CORNERS, "faceVertexIndices")?;
    let total: u64 = counts.iter().map(|&c| c as u64).sum();
    if total != corners.len() as u64 {
        return Err(format!("faceVertexCounts add up to {total}, faceVertexIndices has {}", corners.len()));
    }
    if let Some(i) = corners.iter().find(|&&i| i as usize >= points.len()) {
        return Err(format!("faceVertexIndices names point {i} of {}", points.len()));
    }
    let mut face_start = vec![0u32];
    for c in &counts {
        face_start.push(face_start.last().expect("one") + c);
    }
    let mut creases = Vec::new();
    if let Some(ci) = array(text, "int[] creaseIndices") {
        let idx: Vec<u32> = numbers(ci, 2 * MAX_CREASES, "creaseIndices")?;
        let lens: Vec<u32> = numbers(array(text, "int[] creaseLengths").ok_or("creaseIndices without creaseLengths")?, MAX_CREASES, "creaseLengths")?;
        let sharp: Vec<f32> = numbers(array(text, "float[] creaseSharpnesses").ok_or("creaseIndices without creaseSharpnesses")?, MAX_CREASES, "creaseSharpnesses")?;
        if lens.iter().map(|&l| l as u64).sum::<u64>() != idx.len() as u64 || sharp.len() != lens.len() || lens.iter().any(|&l| l < 2) {
            return Err("creaseLengths, creaseIndices and creaseSharpnesses do not agree".into());
        }
        let mut at = 0usize;
        for (l, s) in lens.iter().zip(&sharp) {
            for w in idx[at..at + *l as usize].windows(2) {
                if creases.len() >= MAX_CREASES {
                    return Err(format!("more than {MAX_CREASES} crease edges"));
                }
                creases.push((w[0].min(w[1]), w[0].max(w[1]), *s));
            }
            at += *l as usize;
        }
        creases.sort_by_key(|c| (c.0, c.1));
    }
    let uv = match array(text, "texCoord2f[] primvars:st") {
        Some(st) => {
            // The array's metadata, right after its closing bracket.
            let open = text.find("texCoord2f[] primvars:st = [").expect("found") + "texCoord2f[] primvars:st = [".len();
            let close = open + st.len();
            let rest = text[close + 1..].trim_start();
            let meta = rest.strip_prefix('(').and_then(|r| r.find(')').map(|e| &r[..e])).unwrap_or_default();
            if !meta.contains("interpolation = \"faceVarying\"") {
                return Err("primvars:st is not faceVarying".into());
            }
            let flat: Vec<f64> = numbers(st, 2 * MAX_CAGE_CORNERS, "primvars:st")?;
            if flat.len() != 2 * corners.len() {
                return Err(format!("primvars:st has {} values for {} corners", flat.len() / 2, corners.len()));
            }
            let udim = udim.unwrap_or_else(|| meta.contains(&format!("doc = \"{}\"", crate::poly::uv::UDIM_MARK)));
            Some(flat.chunks_exact(2).map(|c| [c[0] as f32, crate::poly::uv::v_in(c[1], udim)]).collect())
        }
        None => None,
    };
    Ok(Cage { points, face_start, corners, uv, creases })
}
