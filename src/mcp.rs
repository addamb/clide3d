//! Built-in MCP server so AI agents can drive the running editor.
//!
//! Transport: MCP "Streamable HTTP" on 127.0.0.1, POST-only with plain JSON
//! replies (no SSE stream, no sessions), which the spec allows. The HTTP
//! thread only speaks the protocol; every tool call is forwarded to the UI
//! thread as a `ToolCall` and executed there in `app/tools.rs`, so agent
//! edits go through the same scene, undo history, and redraw as mouse edits.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

pub const DEFAULT_PORT: u16 = 7878;
const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest wait for the editor to answer one call: film renders and bakes
/// can take minutes (render_film allows up to 600 s).
const TOOL_TIMEOUT: Duration = Duration::from_secs(660);

/// MCP content items, e.g. `{"type":"text","text":..}` or an image.
pub type ToolResult = Result<Vec<Value>, String>;

pub struct ToolCall {
    pub name: String,
    pub args: Value,
    pub reply: Sender<ToolResult>,
    /// Who sent it: an agent over MCP, or the editor's own controls (a UI
    /// stroke sends the same request through the same handlers).
    pub source: crate::ledger::Source,
}

/// Starts the server thread. Returns the channel the UI drains each frame
/// and the URL to show the user.
pub fn start(ctx: egui::Context) -> Result<(Receiver<ToolCall>, String), String> {
    let port = std::env::var("CLIDE3D_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            // One thread per request, so a call waiting on a long job (a
            // bake, a film render) does not hold up the next one. Calls
            // still run on the window's thread in the order they arrive.
            for request in server.incoming_requests() {
                let (tx, ctx) = (tx.clone(), ctx.clone());
                let _ = std::thread::Builder::new().name("mcp request".into()).spawn(move || handle(request, &tx, &ctx));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok((rx, format!("http://127.0.0.1:{port}/mcp")))
}

fn handle(mut request: Request, tx: &Sender<ToolCall>, ctx: &egui::Context) {
    if request.url().split('?').next() != Some("/mcp") {
        return respond(request, 404, None);
    }
    // DNS-rebinding guard required by the spec: only local pages may call us.
    let origin = header(&request, "Origin");
    if origin.is_some_and(|o| !is_local_origin(&o)) {
        return respond(request, 403, None);
    }
    if *request.method() != Method::Post {
        // We never open a server-to-client SSE stream.
        return respond(request, 405, None);
    }
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        return respond(request, 400, None);
    }
    let reply = match serde_json::from_str::<Value>(&body) {
        Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        Ok(Value::Array(batch)) => {
            let replies: Vec<Value> = batch.iter().filter_map(|m| dispatch(m, tx, ctx)).collect();
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        Ok(msg) => dispatch(&msg, tx, ctx),
    };
    match reply {
        Some(v) => respond(request, 200, Some(v)),
        // Notifications and client responses get 202 with no body.
        None => respond(request, 202, None),
    }
}

/// Returns `None` for messages that need no reply.
pub(crate) fn dispatch(msg: &Value, tx: &Sender<ToolCall>, ctx: &egui::Context) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method")?.as_str()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = SUPPORTED_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&SUPPORTED_VERSIONS[1]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "clide3d", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Controls the live Clide3D 3D editor window. Units are scene units, \
                    rotations are degrees (XYZ order), colors are sRGB 0..1, Y is up, and a character \
                    faces +Z (the front view). Objects are addressed by numeric id or by name. \
                    To sculpt from an image: set_reference with the image, create_object with shape \
                    Clay and a first set of blobs, then loop: compare_reference (front, then right) and \
                    render_views to see the model, and edit_clay to fix what differs. Use mirror: true \
                    for symmetric parts. Then sculpt for detail and paint / project_reference for color. \
                    To animate a clay character: rig adds bones inside the clay (in its own space, like \
                    blobs; check the weights warnings), animate sets keys (bone rotations in degrees at \
                    time-slider frames), and render_animation shows the motion as a strip of frames. \
                    To build an environment (a dungeon room, a crypt, a cave): kit places walls, corners, doorways, \
                    pillars, floors, rocks, rubble and lit props (torches, candles, braziers) on a 1 m grid; scatter \
                    spreads copies of rocks, bones and candles over the floor; render_film with views [\"iso\"], \
                    fog and a dark environment shows it the way a game would. \
                    To detail a hard-surface asset (a mech, vehicle or weapon) like a AAA game: detail_pass panel, \
                    then bolt, greeble and cable (one call each, mirror: true for symmetry), then a few kitbash hand \
                    placements (pistons, hoses, handles, latches, lights); kitbash action remove undoes a pass. \
                    For symmetric parts use mirror_object (linked: true keeps the copy following edits); to pose props use update_object align / frame / grip \
                    instead of computing axes; to re-pose a rig's rest (an arm), pose_bone carries what rides the bones. \
                    Call get_scene first.",
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_definitions() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let detailed = crate::replies::verbose(&args);
            match call_on_ui_thread(name, args, tx, ctx) {
                Ok(mut content) => {crate::replies::bound(&mut content, detailed); json!({ "content": content, "isError": false })},
                Err(e) => {
                    let mut content=vec![json!({"type":"text","text":e})];
                    crate::replies::bound(&mut content,detailed);
                    json!({"content":content,"isError":true})
                },
            }
        }
        _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn call_on_ui_thread(name: String, args: Value, tx: &Sender<ToolCall>, ctx: &egui::Context) -> ToolResult {
    let (reply, wait) = mpsc::channel();
    tx.send(ToolCall { name, args, reply, source: crate::ledger::Source::Agent }).map_err(|_| "editor is shutting down".to_string())?;
    // eframe sleeps while idle; wake it so the call runs now.
    ctx.request_repaint();
    wait.recv_timeout(TOOL_TIMEOUT).map_err(|_| "editor did not answer in time".to_string())?
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn is_local_origin(origin: &str) -> bool {
    let host = origin.split("://").nth(1).unwrap_or(origin);
    let host = host.split(':').next().unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn respond(request: Request, status: u16, body: Option<Value>) {
    let response = match body {
        Some(v) => Response::from_string(v.to_string())
            .with_header(Header::from_bytes("Content-Type", "application/json").expect("static header")),
        None => Response::from_string(""),
    };
    // A failed write means the client hung up; nothing to do.
    let _ = request.respond(response.with_status_code(status));
}

fn tool_definitions() -> Value {
    let vec3 = |what: &str| json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": what });
    let target = json!({ "type": ["integer", "string"], "description": "Object id (number) or name (string)." });
    let color = json!({ "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 1 }, "minItems": 3, "maxItems": 3, "description": "sRGB [r, g, b], 0..1." });
    let shape = json!({ "type": "string", "enum": ["Cube", "Sphere", "Cylinder", "Cone", "Plane", "Torus", "Group", "Clay", "Eye", "Hair", "Kit", "Terrain"] });
    // Stroke controls shared by sculpt and paint strokes (brush::spec).
    let curve_points = |what: &str| json!({ "type": "array", "maxItems": 16, "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }, "description": what });
    let stroke_controls = json!({
        "points": { "type": "array", "maxItems": 512, "items": { "description": "[x, y, z], [x, y, z, pressure 0..1], or {p: [x, y, z], pressure, normal, t}.", "oneOf": [
            { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 4 },
            { "type": "object", "properties": { "p": vec3("Point."), "pressure": { "type": "number", "minimum": 0, "maximum": 1 }, "normal": vec3("Surface normal (orients spray)."), "t": { "type": "number", "description": "Seconds since the stroke began." } }, "required": ["p"] }] },
            "description": "A path; the brush follows the segments between points. A fourth number is pen pressure." },
        "kind": { "description": "How points become dabs: path (default; dab brushes such as standard space dabs along it, others sweep it), dots (one dab per point), drag_dot (one dab where the drag ends), {spray: {count 1-64, jitter (x radius, default 0.5), seed}} (scattered dabs around the spaced path; the same seed replays the same dabs), {drag_rect: {from, to}} (one dab at from, as wide as the drag), {curve: {closed}} (points are control points: the stroke follows a smooth curve through them, back to the first when closed; the record keeps the control points, so stroke_edit replace with one moved is MoveCurve).",
            "oneOf": [{ "type": "string", "enum": ["path", "dots", "drag_dot"] }, { "type": "object", "properties": {
                "curve": { "type": "object", "properties": { "closed": { "type": "boolean" } } },
                "spray": { "type": "object", "properties": { "count": { "type": "integer", "minimum": 1, "maximum": 64 }, "jitter": { "type": "number", "minimum": 0, "maximum": 4 }, "seed": { "type": "integer", "minimum": 0 } } },
                "drag_rect": { "type": "object", "properties": { "from": vec3("Center."), "to": vec3("Edge.") }, "required": ["from", "to"] } } }] },
        "frame": { "description": "What points are given in: world (default: the call's space), object (the clay's own space), {surface: {offset}} (snapped onto the clay, lifted offset along its normal), {screen: {view: front|back|left|right|top|bottom, at: [[h, v(, pressure)], ..]}} (rays from a named view, world units as in surface_point; replaces points) or {screen: {camera: {view: 16 numbers, proj: 16 numbers (wgpu depth 0..1), viewport: [w, h]}, px: [[x, y(, pressure)], ..]}}.",
            "oneOf": [{ "type": "string", "enum": ["world", "object"] }, { "type": "object" }] },
        "falloff": { "description": "How the dab fades from center to edge: smooth (the classic clay curve), linear, sharp, flat (plateau to 0.9 of the radius), sphere; or [[t, value], ..] (2-16 points, t rising 0..1; a monotone cubic); or {preset or curve, focal_shift -1..1} (+ narrower and pointier, - fuller). Default: the brush's usual curve.",
            "oneOf": [{ "type": "string", "enum": ["smooth", "linear", "sharp", "flat", "sphere"] }, curve_points("[t, value] points."), { "type": "object", "properties": { "preset": { "type": "string" }, "curve": curve_points("[t, value] points."), "focal_shift": { "type": "number", "minimum": -1, "maximum": 1 } } }] },
        "pressure": { "type": "object", "description": "How point pressure scales the dab: size and strength each linear (default), off, or [[pressure, factor], ..]. Ignored when no point has pressure.", "properties": {
            "size": { "oneOf": [{ "type": "string", "enum": ["linear", "off"] }, curve_points("[pressure, factor] points.")] },
            "strength": { "oneOf": [{ "type": "string", "enum": ["linear", "off"] }, curve_points("[pressure, factor] points.")] } } },
        "spacing": { "type": "number", "minimum": 0.01, "maximum": 4, "description": "Dab spacing as a fraction of the radius (default 0.15 for dab brushes; set it to resample other brushes' paths)." },
        "lazy": { "type": "number", "minimum": 0, "maximum": 1, "description": "LazyMouse: the brush trails the path on a string of lazy x radius, smoothing wobbles. 0 is off; otherwise 0.01 to 1." },
        "symmetry": { "type": "object", "additionalProperties": false, "description": "Symmetric copies, applied after the stroke: mirror axes, then radial turns, about the object's pivot (default) or the world origin. mirror: true is axes [x].", "properties": {
            "axes": { "type": "array", "items": { "type": "string", "enum": ["x", "y", "z"] } },
            "radial": { "type": "object", "properties": { "axis": { "type": "string", "enum": ["x", "y", "z"] }, "count": { "type": "integer", "minimum": 1, "maximum": 32 } }, "required": ["axis", "count"] },
            "about": { "type": "string", "enum": ["pivot", "origin"] },
            "posable": { "type": "boolean", "description": "Sculpt only: the points are on the clay as posed at symmetry.frame (default: the editor's frame; a rig's clip): each goes back to the rest pose through the skin weights where it is, the copies are made there, and the rest pose is sculpted. A stroke on a posed left arm with axes [x] also lands on the right arm." },
            "frame": { "oneOf": [{ "type": "number" }, { "type": "string", "enum": ["rest"] }], "description": "Posable only: the pose the points are on, a frame of the rig's active clip or \"rest\". Left out, the editor's frame is used and written into the record and the reply (pose_frame), so resending a stored stroke never depends on the timeline." } } },
        "taper": { "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 1 }, "minItems": 2, "maxItems": 2, "description": "[start, end]: fade the stroke in over the first and out over the last fraction of its length (crease with taper [0.2, 0.2] is the Dam Standard)." },
    });
    // Where a sculpt or paint stroke may work (brush::spec::StrokeMask).
    let stroke_mask = json!({ "type": "object", "additionalProperties": false, "description": "Where the stroke may work. Every stroke also keeps off the clay's persistent mask (mask tool: each point moves by 1 - mask) and hidden polygroups (group tool).", "properties": {
        "blobs": { "type": "array", "items": { "type": "string" }, "description": "Only where these named blobs own the nearest surface; mirrors share names. Empty = nowhere. Unknown names are errors." },
        "above": { "type": "number", "description": "Only at or above this object-local Y, irrespective of stroke space." },
        "below": { "type": "number", "description": "Only at or below this object-local Y. Limits intersect with blobs." },
        "channel": { "type": "boolean", "description": "false ignores the persistent mask (default true)." },
        "invert_channel": { "type": "boolean", "description": "Work where the persistent mask is instead of where it is not." },
        "groups": { "type": "array", "items": { "type": "integer", "minimum": 1 }, "maxItems": 256, "description": "Only in these polygroups." },
        "backface": { "type": "boolean", "description": "Auto-mask: leave surface facing away from the dab alone (the far side of a thin wall). Sculpt only." },
        "normal_angle": { "type": "number", "minimum": 0, "maximum": 180, "description": "Auto-mask: only surface within this many degrees of the dab's normal. Sculpt only." },
        "cavity": { "type": "number", "minimum": -1, "maximum": 1, "description": "Auto-mask: positive spares cavities by that much, negative spares ridges. Sculpt only." },
        "connected": { "type": "boolean", "description": "Auto-mask (Move Topological): only surface joined to the dab's center within 1.5 radii, so a grab moves one finger and not the one beside it. Sculpt only." } } });
    let alpha_schema = json!({ "type": "object", "additionalProperties": false, "description": "An alpha image shaping each dab (dab brushes, paint and mask_paint; those go dab by dab with one), or for brush vdm the VDM stamp it stamps.", "properties": {
        "name": { "type": "string", "description": "An alpha in the scene's library (alpha import / from_view / create_vdm)." },
        "hash": { "type": "string", "description": "Or its content hash (records keep it)." },
        "depth": { "type": "number", "description": "standard, use height: how far white rises over mid (scene units; default strength x radius / 2)." },
        "mid": { "type": "number", "minimum": 0, "maximum": 1, "description": "The gray that stays put (default 0); with 0.5 black sinks." },
        "rotate": { "type": "number", "description": "Degrees on the surface." },
        "scale": { "type": "number", "minimum": 0.05, "maximum": 20, "description": "The image spans scale x the dab's diameter (default 1)." },
        "mode": { "type": "string", "enum": ["stamp", "drag", "tile"], "description": "stamp: upright on the object; drag: turned with the stroke; tile: repeated across the surface (not with brush vdm, which also takes no depth, mid or use)." },
        "use": { "type": "string", "enum": ["height", "mask"], "description": "height (standard's default): the dab's profile; mask (the others' default): a weight." } } });
    let with_controls = |props: Value| {
        let mut props = props;
        let m = props.as_object_mut().expect("properties object");
        for (k, v) in stroke_controls.as_object().expect("controls object") {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
        props
    };
    let light = json!({ "description": "A light this object gives off (any shape; torch, candle and brazier pieces come with one). A preset name (candle, torch, brazier, lantern, fire, magic), settings merged onto the current light (preset, offset: where it sits in the object's space, temperature: Kelvin, 1800 candle .. 6500 daylight, color: linear RGB instead of temperature, intensity: about the light a surface 1 m away gets (studio key = 3), radius: meters, bigger = softer shadows, flicker: 0..1, every copy and frame differs), or null to remove. render_film lights the scene with every one, scattered copies included, and the viewport shows their light pools.",
        "oneOf": [{ "type": "string" }, { "type": "object" }, { "type": "null" }, { "type": "boolean" }] });
    let kit_settings = json!({ "type": "object", "description": "Kit only: piece settings, merged (see the kit tool)." });
    let terrain_settings = json!({ "type": "object", "description": "Terrain only: settings, merged (see edit_terrain): size [w, d], resolution, height, frequency, octaves, ridged, cracks, crack_scale, crack_width, seed, color, rock_color." });
    let hair = json!({
        "type": "object",
        "description": "Hair only (merged into the current settings). guides: curves the hair follows, each a list of points root first (or {points, width}); guides replaces the list, add_guides appends. \
            Put roots on the scalp (find them with surface_point) and run each guide out and down the way that lock falls; 20 to 60 guides make a full head with mirror. \
            width (maximum card root width, default 0.03; scalp cards adapt to root spacing, guide.width overrides), taper (tip width / root width, default 0.35), cards (strips per guide budget, default 3; 48 triangles per card), spread (how far extra strips sit from their guide, default 0.008), \
            root_color / tip_color (sRGB), center (the head's middle: cards face away from it; default the middle of the roots), mirror (also use every guide mirrored across X = 0), seed. \
            render_film draws real strands instead of cards: strands (budget, default 30000; fewer near open hairline edges), strand_width (default 0.00008), frizz (waviness at the tip, default 0.002; outer strands are looser). \
            Volume and locks (strands, and the cards' layers and texture, follow them): volume (thickness of the hair layer in meters, default 0.008: strands rise off the scalp at the root and lie in layers up to this far out, the outer ones sparser; 0.01-0.02 for full long hair, 0.003 for slicked hair), \
            clump (0..1, how tightly strands gather into locks toward the tips, default 0.5; 0.4-0.6 looks natural, 0.8 wet or styled), clump_size (lock width at the root in meters, default 0 = the guide spacing), clump_levels (1-3, default 2: locks split into smaller locks inside them), \
            flyaways (share of stray strands that leave the surface, 0..0.3, default 0.015), length_variation (0..1, default 0.15: some strands shorter, for soft uneven ends), melanin_variation (0..1, default 0.25: per-strand pigment, some hairs darker or lighter in the same hue, with slightly sun-lightened tips and outer layer), rise (0..2, default 1: how far blended strands arch off the scalp after the root; peach fuzz uses 0.02). \
            render_film calibrates hair so a full head looks the root_color / tip_color you set (not darker). \
            Strands fill scalp triangles between multiple guides; roots follow the curved scalp, with finer shorter edge hairs and soft scalp pigment under dense roots in film. Place guides evenly and set center to the head middle. Replies include card_triangles, root_spacing, largest_root_gap, sparse_patches, warnings and a sampled strands_inside_head count. Unknown hair/grow keys are errors; grow.region is invalid, put center/radii directly in grow. Long hair: grow count 80, length 0.3, lift 0.002; strands 60000, strand_width 0.00008, clump 0.5, clump_levels 3, volume 0.014, frizz 0.0018, flyaways 0.02, cards 4, width 0.025, spread 0.004. Short hair: count 100, length 0.035, lift 0.001; strands 40000, strand_width 0.00007, clump 0.1, frizz 0.0004, cards 3, width 0.015, taper 0.1. Braids: draw distinct winding guides around a braid axis, interpolate:false, spread 0.001, clump 0.4, frizz 0.0003, cards 3, width 0.01; use a separate interpolated scalp groom for the crown. All lengths are meters; more strands may be needed for close-ups. \
            grow makes the guides for you: {on: the head clay (required), count (default 40), length (default 0.3), comb [x,y,z] (the way the hair falls, default down and a little back), part (x of the parting line), center / radii (world ellipsoid of scalp to cover; default hairline to nape, face left bare), lift (root height, default 0.0015), mirror (grow only one side, for hair with mirror: true), replace (default true)}. Roots spread evenly; each guide drapes like a hanging chain: combed along the head while it touches it, then falling onto the shoulders and back (the body clay the head sits on). Good for long hair. For facial hair use style: brow (short, surface-combed, no gravity) or lash (leaves the lid and curls toward comb). These require a small world center/radii region, or roots: [[x,y,z],...] placed along the brow or eyelid with surface_point. Defaults: length 0.007 m, lift 0.0001, width 0.0005, cards 1, strand_width 0.00006, taper 0.05, spread/frizz 0.00005, strands 600 (brow) or 120 (lash); explicit hair settings override them. comb is the world direction along the brow or the lash curl (e.g. [1,0.4,0] brow, [0,1,0] upper lashes). For lashes prefer explicit roots, one per lash; use strands close to the root count. Facial styles also default interpolate: false to keep individual guides distinct. Erase painted brow/lash strokes first. Parent/bone it to the head; mirror uses the hair object's local X plane.",
    });
    let material = json!({ "type": ["string", "null"], "description": "Material name: how the surface reflects light (the color stays). Presets: clay (default), skin, lips, cloth, velvet, leather, scales, hair, wood, plastic, rubber, metal, polished_metal, gloss, glow (gives off light in its color), glass, water, crystal (see-through); \
    smart presets (layered looks with their own colors, wear, rust and grime: see set_material): worn_iron, rusted_iron, polished_steel, gold_trim, bronze, old_leather, cracked_leather, bone, charred_flesh, wet_flesh, demon_skin, dungeon_stone, mossy_stone, grimy_cloth, blood_splatter; \
    or one made with set_material. On Clay it is the default for blobs that name none. null = clay." });
    let eye = json!({
        "type": "object",
        "description": "Eye only (merged into the current settings): iris_color, sclera_color, pupil_color (sRGB 0..1), iris_size (iris width / eye width, default 0.5; create_human eyes use about 0.41), pupil_size (pupil width / iris width, default 0.34), cornea (bulge, default 0.05), veins (red veins in the white, 0..1, default 0.4), limbal_ring (dark ring around the iris, 0..1, default 0.6). The eye is textured (iris fibers, crypts, collarette, sclera veins) with the iris set back under a clear refracting cornea. create_human eyes also carry lids (where the lids meet the eye): the white darkens under the lids, a wet tear line runs along the lower lid and a pink caruncle fills the inner corner.",
        "properties": {
            "iris_color": color.clone(), "sclera_color": color.clone(), "pupil_color": color.clone(),
            "iris_size": { "type": "number", "minimum": 0.05, "maximum": 0.9 },
            "pupil_size": { "type": "number", "minimum": 0.05, "maximum": 0.95 },
            "cornea": { "type": "number", "minimum": 0, "maximum": 0.2 },
            "veins": { "type": "number", "minimum": 0, "maximum": 1 },
            "limbal_ring": { "type": "number", "minimum": 0, "maximum": 1 },
        },
    });
    let skin = json!({
        "type": "object",
        "description": "Face skin detail of a human head (film renders and bakes; on by default). Pores sized by region (fine on the forehead, medium on the cheeks, large on the nose), wrinkles along real expression lines (forehead lines, frown lines, crow's feet, under-eye creases, nasolabial folds, marionette and upper-lip lines) that deepen with age, a fine cross-hatch of skin lines, an oily T-zone with a thin glossy sheen, redness on nose, cheeks, chin and ears, a cool tint under the eyes, and shaped lips (rose vermilion, vertical lip lines, moist highlight, soft border). Merged into the current settings. Example: {age: 35, pores: 1, wrinkles: 0.6, freckles: 0.2, redness: 0.4}.",
        "properties": {
            "enabled": { "type": "boolean", "description": "Face detail on (default true)." },
            "age": { "type": "number", "description": "Skin age in years for wrinkles (default: the human's age)." },
            "pores": { "type": "number", "minimum": 0, "maximum": 2, "description": "Pore depth (default 1)." },
            "wrinkles": { "type": "number", "minimum": 0, "maximum": 2, "description": "Wrinkle depth on top of age (default 0.6)." },
            "freckles": { "type": "number", "minimum": 0, "maximum": 1, "description": "Freckles on nose, cheeks and forehead (default 0)." },
            "moles": { "type": "number", "minimum": 0, "maximum": 1, "description": "0..1 gives 0..8 moles (default 0)." },
            "redness": { "type": "number", "minimum": 0, "maximum": 1, "description": "Default 0.35." },
            "under_eye": { "type": "number", "minimum": 0, "maximum": 1, "description": "Cool, darker skin under the eyes (default 0.35)." },
            "oil": { "type": "number", "minimum": 0, "maximum": 1, "description": "T-zone shine (default 0.5)." },
            "stubble": { "type": "number", "minimum": 0, "maximum": 1, "description": "Beard shadow of cut hairs (default 0)." },
            "fuzz": { "type": "number", "minimum": 0, "maximum": 1, "description": "Peach-fuzz sheen (default 0.5); real fuzz strands come from face_hair." },
            "lip_color": color.clone(),
            "lips": { "type": "number", "minimum": 0, "maximum": 1, "description": "How strongly lips are drawn (default 1)." },
            "seed": { "type": "integer", "description": "Changes freckles, moles and line breaks." },
        },
    });
    let aim = vec3("Turn the object so its front (local +Z; an eye's pupil) points at this world point, up kept near +Y. Applied after translate/rotate.");
    let blob = json!({
        "type": "object",
        "description": "One clay blob, in the clay object's own space. Blobs apply in order. Forms by type: \
            sphere {center, radius}; ellipsoid {center, radii [x,y,z], rotate}; capsule {a, b, radius, radius_b} (a rounded, \
            optionally tapered tube from a to b: limbs, fingers, neck, tail); box {center, size [w,h,d], rotate, round}; \
            torus {center, radius, thickness, rotate} (ring in the XZ plane); membrane {points, thickness, sag, scallop, scallop_edges} (thin wing polygon). chain {points, size [width,length,depth], twist, taper, tilt} makes connected beads along an arc-length sampled path (braids, necklaces, plate tails). twist is degrees per bead; taper is end/start size ratio (default 1); tilt leans beads left and right in turn (a braid: tilt 30-40, twist 0, size width about 1.6 x depth). rotate is degrees [x,y,z]. Hard surfaces: extrude {profile:[[u,v],...],depth,center,axis,bevel} (default axis +Z; +Z uses XY profile); lathe {profile:[[radius,height],...],center,axis} (default +Y); sweep {profile,points,taper,twist,path_smooth} (local XY section, transported frame, total twist degrees); shell {source: form,thickness} hollows inward; array {source: form,count,step} repeats along a line, radius/center/axis selects a circle, points selects an arc-length path. Profiles are simple polygons, either winding, 3-128 points; a corner may be {point:[u,v],radius} for a circular fillet, or {point:[u,v],smooth:true} for spline interpolation (especially lathe domes; ordinary points keep sharp transitions). Sweep paths are smooth splines by default; path_smooth:false retains a polygonal path. Arrays have 1-512 copies, nested total <=4096. For many repeated modules (tower floors, railings, columns) call instance_array afterwards, so one copy is meshed and every copy is drawn, rendered and exported as an instance of it. band {points,width,normal} with op engrave/emboss and wall depth makes a constant-depth surface groove/band; points lie on the surface, normal is its projection direction. Use separate bands for different sides/normal directions. Form wrappers contain geometry only; color/material/bone/blend belong on the outer blob. Rivets 0.004-0.006 m, plate wall 0.003-0.005 m, blade bevel 0.001 m. Resolve thickness/width with at least two mesh cells; use separate small clay objects or detail_cell for tiny parts. Sharp feature placement is automatic; sharp:false in edit_clay gives the old mesh for comparisons.",
        "properties": {
            "type": { "type": "string", "enum": ["sphere", "ellipsoid", "capsule", "box", "torus", "membrane", "chain", "extrude", "lathe", "sweep", "shell", "array", "band", "mesh"] },
            "asset": { "type": "string", "description": "mesh: a triangle mesh by content hash (get_clay lists a clay's; import_mesh and to_clay make them). With sign (closed: the pseudo normals, default; winding: the generalized winding number, for open meshes: holes close), center, rotate and scale (uniform)." },
            "name": { "type": "string", "description": "Label to address the blob later." },
            "op": { "type": "string", "enum": ["add", "carve", "cover", "plate", "engrave", "emboss", "intersect"], "description": "add melts it on (default); carve cuts it away (eye sockets, mouths); intersect keeps only what is inside both it and everything before it (blend rounds the cut); cover makes clothing: everything before this blob, grown by offset, kept only inside this blob's shape (a box over the torso = a fitted tunic, a capsule around the upper arm = a sleeve, around the shin = a boot). Covers fit the body exactly, have a crisp edge (blend rounds it; use about 0.004) and their own color and material, and move with the rig. For loose parts (a flared skirt, a cape) use add blobs. plate fits a filtered body shell with a hollow wall (default 0.004), offset clearance and rim bevel, clipped to this shape. Use smooth:0.08-0.14 for a forged breastplate that bridges anatomy; 0.03-0.06 for greaves/vambraces; 0 keeps an exact anatomical fit. ridge adds a center crest and flare turns the limiting edge outward (meters). Set bone to a deform bone for rigid armor, including split rigs. engrave/emboss require band and use wall as constant depth; blend 0 gives crisp walls." },
            "smooth": { "type": "number", "minimum": 0, "maximum": 0.5, "description": "cover/plate: smoothing footprint in object units, with clearance to conceal bumps and bridge pits (0 = exact fitted behavior). Tunic 0.02-0.04; boots 0.02-0.05. Forged torso plate 0.08-0.14; limb plates 0.03-0.06. Plate fitting uses a cached broad filter with smooth reconstruction and sampled conservative clearance; earlier plate operations do not inflate the fitting body again. Increase the limiting blob to contain the loose garment." },
            "loose": { "type": "number", "minimum": 0, "maximum": 0.5, "description": "Alias for cover/plate smooth; supply only one." },
            "twist": { "type": "number", "description": "chain: rotation about its tangent in degrees per bead; sweep: total twist along arc length." },
            "tilt": { "type": "number", "minimum": -80, "maximum": 80, "description": "chain: degrees each bead leans across the path, alternating sides (braid strands: 30-40)." },
            "taper": { "type": "number", "exclusiveMinimum": 0, "maximum": 10, "description": "End/start size ratio (default 1). sweep accepts (0,10]; chain clamps to 0.05-2 and joins up to 512 beads by a narrow core." },
            "offset": { "type": "number", "minimum": 0, "description": "cover: how far the garment stands off the body (default 0.004; boots 0.006+, coats 0.01+)." },
            "blend": { "type": "number", "minimum": 0, "description": "Join radius with earlier blobs. Default 0.1; 0 is a hard union/subtraction." },
            "join": { "type": "string", "enum": ["sharp","chamfer","round"], "description": "add/carve join: sharp ignores blend; round (default) smooths; chamfer joins by a flat diagonal plane." },
            "join_size": { "type": "number", "minimum": 0, "description": "Join size; defaults to blend. Distinct from a box's size vector." },
            "wall": { "type": "number", "exclusiveMinimum": 0, "description": "plate full thickness or engrave/emboss constant depth; default 0.004 m." },
            "rim": { "type": "number", "minimum": 0, "description": "plate rim bevel footprint; keep below half wall thickness." },
            "ridge": { "type": "number", "minimum": 0, "description": "plate center crest height, local X center of limiter; 0.01-0.02 m on a breastplate." },
            "flare": { "type": "number", "minimum": 0, "description": "plate outward lip flare at the limiting edge, over four wall thicknesses; typically 0.003-0.008 m. Add a sweep trim for a rolled lip." },
            "profile": { "type": "array", "minItems": 3, "maxItems": 128, "description": "Simple ordered polygon: [u,v] or {point:[u,v],radius}; lathe uses radius/height." },
            "depth": { "type": "number", "exclusiveMinimum": 0, "description": "extrude full depth, centered on center." },
            "axis": vec3("extrude axis default +Z, lathe/circular array axis default +Y; nonzero."),
            "bevel": { "type": "number", "minimum": 0, "description": "Extrusion edge bevel radius inside the original bounds." },
            "source": { "type": "object", "description": "Nested geometry form for shell/array, including its type tag." },
            "count": { "type": "integer", "minimum": 1, "maximum": 512 },
            "step": vec3("array translation per copy; circle radius or points path overrides step."),
            "width": { "type": "number", "exclusiveMinimum": 0, "description": "band full surface width." },
            "normal": vec3("band projection direction, default +Z; use surface_point to place its path."),
            "color": color.clone(),
            "mirror": { "type": "boolean", "description": "Also apply it mirrored across X = 0 (both arms, both eyes)." },
            "color_blend": { "type": "number", "minimum": 0, "description": "Width of the color fade where this blob meets earlier blobs (default: blend). 0 = a crisp color edge (sleeve end, boot top, collar) while the shape still blends smoothly." },
            "material": { "type": "string", "description": "Material name (see create_object material). Empty = the object's material. Where blobs blend, the one that wins (over half) gives its material." },
            "bone": { "type": "string", "description": "Weight this blob only to this deform bone, with neighboring blends at seams. Mirrored blobs swap side suffixes. Empty = automatic." },
            "points": { "type": "array", "minItems": 2, "maxItems": 128, "items": vec3("Object-space point. Membrane: 3-20 ordered boundary points, simple projected polygon; winding sets sag. Chain: 2-128 path points, sampled evenly along arc length.") },
            "sag": { "type": "number", "description": "Membrane bow along its winding normal, zero at edges (default 0)." },
            "scallop": { "type": "number", "minimum": 0, "maximum": 0.5, "description": "Membrane cut-in depth as a fraction of each flagged edge length (default 0)." },
            "scallop_edges": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "description": "Membrane edge indices: edge i goes from point i to the next, wrapping to point 0." },
            "center": vec3("sphere/ellipsoid/box/torus center."),
            "radius": { "type": "number" },
            "radius_b": { "type": "number", "description": "capsule: radius at b (default radius)." },
            "a": vec3("capsule start."),
            "b": vec3("capsule end."),
            "radii": vec3("ellipsoid radii."),
            "size": vec3("box full size; chain bead full width, length along path, depth."),
            "round": { "type": "number", "description": "box edge rounding." },
            "thickness": { "type": "number", "description": "torus tube radius; membrane full thickness or shell inward wall thickness (e.g. 0.006). Use at least two mesh grid cells for a reliable sheet." },
            "rotate": vec3("Rotation in degrees."),
        },
        "required": ["type"],
    });
    let blob_key = json!({ "type": ["integer", "string"], "description": "Blob index or name." });
    let view = json!({ "description": "A name (front, back, right, left, top, bottom, three_quarter, three_quarter_back, iso: a Diablo-style high three-quarter view, yaw 45, pitch 52, long 18-degree lens) or {yaw, pitch, ortho, fov}; {view: \"iso\", yaw: 135} turns a named view. Yaw 0 = camera on +Z looking at the front, 90 = camera on +X. Either may be an object with its own close-up: {view: \"front\", center: [x,y,z], radius} or {yaw, pitch, center, radius} (so one picture can show the head and a flank)." });
    let empty = json!({ "type": "object", "properties": {} });
    let tool = |name: &str, description: &str, mut schema: Value| {
        if schema["properties"].is_null(){schema["properties"]=json!({});}
        let properties=schema["properties"].as_object_mut().expect("tool properties");
        properties.entry("verbose").or_insert(json!({"type":"boolean","description":"Return full detail; default text replies are limited to about 8k characters."}));
        properties.entry("detail").or_insert(json!({"type":"boolean","description":"Return full detail (same as verbose)."}));
        if matches!(name,"get_scene"|"open_scene"|"set_material"|"get_clay") {
            for (key,value) in [
                ("filter",json!({"type":"string","description":"Name glob (* and ?)."})),
                ("offset",json!({"type":"integer","minimum":0})),
                ("limit",json!({"type":"integer","minimum":1})),
                ("all",json!({"type":"boolean","description":"Include children as well as top-level objects."})),
                ("parent",json!({"description":"Filter direct children by parent id/name; null for roots."})),
                ("target",json!({"description":"Filter a single object by id/name."})),
            ] {properties.entry(key).or_insert(value);}
        }
        if name=="set_material" {
            for (key,ty) in [("list","boolean"),("unused","boolean"),("delete_unused","boolean"),("replace","string"),("with","string"),("rename","string"),("replace_with","string")] {
                properties.insert(key.into(),json!({"type":ty}));
            }
        }
        if name=="job_status" {
            properties.insert("offset".into(),json!({"type":"integer","minimum":0}));
            properties.insert("limit".into(),json!({"type":"integer","minimum":1}));
        }
        if name=="export_glb" {
            properties.insert("atlas".into(),json!({"type":"boolean","default":true,"description":"Pack baked parts into shared color/normal/ORM atlases within check_export limits; false keeps original maps."}));
            properties.insert("texel_density".into(),json!({"type":"number","exclusiveMinimum":0,"description":"Atlas target pixels per world meter; automatically reduced to fit the checker budget."}));
            properties.insert("instancing".into(),json!({"type":"string","enum":["nodes","ext"],"default":"nodes","description":"How scattered copies (and instance_array copies) are written: nodes (default) gives each copy a node pointing at one shared mesh; ext writes one node per source with EXT_mesh_gpu_instancing transforms (smaller; readers without the extension show one copy). Identical objects share meshes either way."}));
        }
        if name=="kitbash" {
            properties.insert("route".into(),json!({"type":"boolean","default":true,"description":"Route flexible spans outside targets, with sag between authored clips. Pistons warn on intersections. action:reattach accepts from/to world endpoints and bones."}));
            if let Some(actions)=properties.get_mut("action").and_then(|a|a["enum"].as_array_mut()){actions.push(json!("reattach"));}
        }
        json!({"name":name,"description":description,"inputSchema":schema})
    };

    let film_environment = json!({ "description": "Image lighting (default studio). Skies include ground bounce and an optional finite sun disc; HDR is linear Radiance RGBE, equirectangular. Use lights none for natural illumination. Presets also accept {preset, intensity, rotate} to adjust sky fill independently of explicit lights. physical is a physical sun and sky (single Rayleigh, Mie and ozone scattering): the sun and sky redden and dim as sun_elevation drops, the sun is a 0.53 degree disc sampled directly; sky_noon, sky_afternoon, sky_golden and sky_sunset are physical at 60, 30, 8 and 1.5 degrees. Its brightness is calibrated so a 30 degree sun lights the ground like golden_hour. Fog does not follow the sky: set fog color to match.", "oneOf": [
            { "type": "string", "enum": ["studio", "overcast", "sunset", "forest", "golden_hour", "night", "storm", "physical", "sky_noon", "sky_afternoon", "sky_golden", "sky_sunset"] },
            { "type": "object", "oneOf": [{"required":["hdr"]},{"required":["preset"]}], "properties": {
                    "hdr": { "type": "string", "description": "Absolute .hdr path, up to 16 million pixels." },
                    "preset": {"type":"string","enum":["studio","overcast","sunset","forest","golden_hour","night","storm","physical","sky_noon","sky_afternoon","sky_golden","sky_sunset"]},
                    "sun_elevation": { "type": "number", "minimum": -5, "maximum": 90, "description": "Physical sky only: degrees above the horizon (preset's default; physical 30). Below 0 leaves twilight and no sun." },
                    "sun_azimuth": { "type": "number", "minimum": -36000, "maximum": 36000, "default": 39.29, "description": "Physical sky only: degrees from +Z toward +X (default: render_film's key light, [0.45, 0.75, 0.55]). rotate turns it further." },
                    "turbidity": { "type": "number", "minimum": 1, "maximum": 10, "default": 2.5, "description": "Physical sky only: haze, 1 clear air to 10 thick haze (whiter sky, wider glow round the sun, redder low sun)." },
                    "ground_albedo": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.3, "description": "Physical sky only: brightness of the ground below the horizon." },
                    "rotate": { "type": "number", "minimum": -36000, "maximum": 36000, "default": 0, "description": "Degrees around world +Y." },
                    "intensity": { "type": "number", "minimum": 0, "maximum": 10000, "default": 1 }
                }, "additionalProperties": false }
        ] });
    let film_lights = json!({ "description": "Default studio (three soft lights); none for only environment lighting; an array replaces studio lights. Linear RGB color. Sun direction points toward light, size is angular diameter in degrees (default 0.53), intensity is integrated radiance/irradiance. Area is a one-sided disc: position, outward direction (default [0,-1,0]), size is diameter in meters (default 1), intensity is radiance. Point uses position and inverse-square intensity. color/intensity default [1,1,1]/1.", "oneOf": [
            { "type": "string", "enum": ["studio", "none"] },
            { "type": "array", "maxItems": 64, "items": { "type": "object", "required": ["type"], "properties": {
                        "type": { "type": "string", "enum": ["sun", "area", "point", "sphere"] }, "direction": vec3("Sun: toward light; area: emitting normal."),
                        "position": vec3("World meters (required for area/point/sphere)."), "color": vec3("Nonnegative linear RGB."),
                        "intensity": { "type": "number", "minimum": 0, "maximum": 100000 }, "size": { "type": "number", "minimum": 0.0001 },
                        "radius": { "type": "number", "minimum": 0.0001, "description": "Sphere: ball radius in meters (soft shadows); intensity as a point light's." }
                    }, "additionalProperties": false } }
        ] });
    let film_extra_lights = json!({ "type": "array", "maxItems": 64, "description": "Optional additional lights using the same objects as lights; appended to studio, none or the replacement array.", "items": { "type": "object", "required": ["type"], "properties": {
                "type": { "type": "string", "enum": ["sun", "area", "point", "sphere"] }, "direction": vec3("Sun: toward light; area: emitting normal."),
                "position": vec3("World meters."), "color": vec3("Linear RGB, default [1,1,1]."), "intensity": { "type": "number", "minimum": 0, "maximum": 100000 }, "size": { "type": "number", "minimum": 0.0001 },
                "radius": { "type": "number", "minimum": 0.0001 }
            }, "additionalProperties": false } });
    let mut film_camera = json!({ "type": "object", "description": "Default pinhole, existing 30-degree vertical FOV. Changing FOV/focal length dollies to retain framing unless position is supplied. Full-frame 24 mm sensor height; scene units are meters. DOF requires a perspective view (ortho false).", "properties": {
            "fov": { "type": "number", "minimum": 1, "maximum": 150, "description": "Vertical degrees, takes precedence over focal_length_mm." },
            "focal_length_mm": { "type": "number", "minimum": 8, "maximum": 1000 },
            "position": vec3("Override world camera position."), "target": vec3("Override world look-at point."),
            "aperture": { "type": "number", "minimum": 0.7, "maximum": 64, "description": "F-stop; omitted means pinhole. Focus defaults to framed center." },
            "dof": { "type": "object", "properties": {
                    "f_stop": { "type": "number", "minimum": 0.7, "maximum": 64, "description": "Overrides aperture; smaller numbers give more blur." },
                    "focus_distance": { "type": "number", "minimum": 0.0001, "maximum": 100000, "description": "Meters along the camera's forward axis; use this or focus_point." },
                    "focus_point": vec3("World point, e.g. the eyes; must be in front of camera.")
                }, "additionalProperties": false }
        }, "additionalProperties": false });
    let mut film_bloom = json!({ "description": "Energy-conserving HDR glare (removes scattered energy from highlight cores), default off. true = strength 0.08, threshold 1 after exposure; a number sets strength.", "oneOf": [
            { "type": "boolean" }, { "type": "number", "minimum": 0, "maximum": 1 },
            { "type": "object", "properties": { "strength": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.08 }, "threshold": { "type": "number", "minimum": 0, "maximum": 100, "default": 1 } }, "additionalProperties": false }
        ] });
    film_camera["properties"].as_object_mut().unwrap().extend(json!({
        "sensor_size_mm": {"type":"array","items":{"type":"number","minimum":1,"maximum":100},"minItems":2,"maxItems":2,"description":"[width,height] in mm, default [36,24]. Supplies aspect when aspect is omitted; explicit aspect crops the sensor. Use this or sensor_height_mm."},
        "sensor_height_mm": {"type":"number","minimum":1,"maximum":100,"description":"Sensor height, default 24 mm; width follows output aspect."},
        "iso": {"type":"number","minimum":1,"maximum":102400,"description":"Physical exposure ISO, default 100 when shutter_seconds is used. Uses aperture, default f/8 for pinhole. EV100 12 gives multiplier 1."},
        "shutter_seconds": {"type":"number","minimum":0.00001,"maximum":30,"description":"Exposure time; default 1/60 when ISO is used. Brightness only; motion_blur controls the animation shutter."},
        "ev": {"type":"number","minimum":-10,"maximum":30,"description":"EV100 exposure setting, 12 = multiplier 1, 11 = twice as bright. Use EV or ISO/shutter."},
        "distortion": {"type":"number","minimum":-0.3,"maximum":0.3,"description":"Radial lens distortion; subtle barrel -0.02, pincushion +0.02. Shared HDR resampling."},
        "chromatic_aberration": {"type":"number","minimum":0,"maximum":0.02,"description":"Radial red/blue separation as a fraction of image radius, subtle 0.001."}
    }).as_object().unwrap().clone());
    film_camera["properties"]["dof"]["properties"].as_object_mut().unwrap().extend(json!({
        "blades":{"type":"integer","description":"0 = circular aperture (default), or 3..16 for polygonal bokeh and matching glare streaks."},
        "blade_rotation":{"type":"number","minimum":-360,"maximum":360,"description":"Aperture rotation in degrees."},
        "focus_object":{"description":"Object name or id: focus at the world center of its mesh bounds, evaluated at each shutter time. Use one of focus_object, focus_point, focus_distance."}
    }).as_object().unwrap().clone());
    film_bloom["oneOf"][2]["properties"].as_object_mut().unwrap().extend(json!({
        "streaks":{"type":"number","minimum":0,"maximum":1,"description":"Fraction of the glare in aperture-shaped star streaks (default 0)."},
        "dirt":{"type":"number","minimum":0,"maximum":1,"description":"Deterministic procedural lens dirt modulates glare; total HDR energy is conserved."}
    }).as_object().unwrap().clone());
    let film_white_balance = json!({ "description": "Default daylight (6500 K). Kelvin temperature to neutralize, or positive linear RGB gains.", "oneOf": [{ "type": "number", "minimum": 2000, "maximum": 12000 }, vec3("RGB gains in (0,10].")] });

    let mut film_schema = json!({
        "type": "object",
        "properties": {
            "views": { "type": "array", "maxItems": 4, "items": view.clone(), "description": "Default [three_quarter]." },
            "center": vec3("Frame this point (default: the whole scene)."),
            "radius": { "type": "number", "description": "Frame this radius around center." },
            "size": { "type": "integer", "minimum": 64, "maximum": 1600, "description": "Pixels per side (default 640): the width when aspect is set." },
            "aspect": { "type": "number", "minimum": 0.25, "maximum": 4, "description": "Width / height (default 1): 1.78 for a 16:9 game screenshot." },
            "fog": { "description": "Mist in the air: true, a density (per meter), or {density (default 0.06), color (scattering albedo 0..1, default [0.9,0.9,0.9]), height (y below which it is densest, default 0), falloff (meters it thins over above height; 0 = even everywhere; default 1.5), anisotropy (-0.95..0.95; > 0 glows around lights, default 0.35), distance (rays leaving the scene pass this much fog, default 40)}. Lights scatter in it (halos, shafts through gaps); CPU and GPU alike. Light mist in a room: {density: 0.05, height: 0.3, falloff: 1}.", "oneOf": [{ "type": "boolean" }, { "type": "number" }, { "type": "object" }] },
            "object_lights": { "type": "boolean", "description": "Light the scene with lights carried by objects (torches, candles, their scattered copies), default true. Many lights stay fast: beyond four, two are picked per point by how much they give." },
            "samples": { "type": "integer", "minimum": 1, "maximum": 4096, "description": "Rays per pixel (default 128); more = less grain." },
            "bounces": { "type": "integer", "minimum": 1, "maximum": 12, "description": "Light bounces (default 4)." },
            "time_limit": { "type": "number", "minimum": 1, "maximum": 600, "description": "Seconds for the whole render, mesh building included (default 90); stops adding samples when reached. The views take turns, so each gets an equal share of samples. Runs as a background job: after 40 s the call answers with a job id; get the picture with job_status (wait: 45)." },
            "floor": { "type": "boolean", "description": "A floor under the scene that catches shadows (default true)." },
            "exposure": { "type": "number", "description": "Brightness multiplier (default 1)." },
            "denoise": { "type": "boolean", "description": "Remove grain (default true)." },
            "denoiser": { "type": "string", "enum": ["variance", "classic"], "description": "How grain is removed: variance (default) is guided by each pixel's measured noise, so clean areas and edges stay sharp and noisy ones are smoothed harder; classic is the previous filter, whose strength follows only the sample count." },
            "frame": { "type": "number", "description": "Pose rigs at this time-slider frame (default: the rest pose)." },
            "baked": { "type": "boolean", "description": "Render baked game meshes instead of clay." },
            "fine": { "type": "boolean", "description": "Render clay with a detail_cell at that fine size (default true)." },
            "fine_wait": { "type": "number", "minimum": 0, "maximum": 600, "description": "Seconds the render waits for fine meshes still building (default half of time_limit, at most 30). The wait runs in the render job, not the tool call, and counts against time_limit. Objects not ready render at their regular mesh and are listed in fine_mesh_errors; their builds go on, so a later render uses them." },
            "save_path": { "type": "string", "description": "Also save the picture to this absolute .png path." },
            "passes": { "type": "array", "maxItems": 7, "items": { "type": "string", "enum": ["depth", "normal", "albedo", "ao", "shadow", "mask", "emission"] }, "description": "Also write render passes for compositing (like ZBrush BPR passes), from the camera's first hits of the same scene on the CPU: depth (distance along the view axis), normal (shading normal), albedo (color without light; the floor shows its gray 0.42, the background 0), ao (1 = open), shadow (1 = blocked from the lights, or the studio key with lights none), mask (object silhouette, anti-aliased; floor and background 0), emission. Files <pass>.png|exr in passes_dir (<view>_<pass> with several views, whose names must make distinct file names); the reply's passes maps each name to its path ({name: {view: path}} with several views), passes_info says how to decode, and a second image shows them. They are traced after the picture, outside time_limit (well under a second at 640 px). Not with motion_blur." },
            "passes_dir": { "type": "string", "description": "Absolute folder for the passes (made when the render starts, if missing); required with passes." },
            "passes_format": { "type": "string", "enum": ["png", "exr"], "description": "png (default: 16-bit for depth, normal, ao, shadow, mask; 8-bit sRGB for albedo and emission) or exr (32-bit floats: depth in world units, linear colors)." },
            "pass_samples": { "type": "integer", "minimum": 1, "maximum": 64, "description": "Anti-aliasing samples per pixel for the passes (default 8); ao casts 8 rays per sample." },
            "ao_distance": { "type": "number", "minimum": 0, "maximum": 1000, "description": "How far the ao pass looks for occluders, in world units (default a quarter of the framing radius)." },
            "normal_space": { "type": "string", "enum": ["world", "camera"], "description": "The normal pass's space (default world; camera: x right, y up, z toward the camera)." },
        },
    });
    let film_lighting = json!({
        "environment": film_environment,
        "background": { "description": "Show environment behind subject (environment/true), or the plain studio backdrop (studio/false, default). Floor remains lit by the chosen environment.", "oneOf": [{ "type": "boolean" }, { "type": "string", "enum": ["studio", "environment"] }] },
        "lights": film_lights,
        "extra_lights": film_extra_lights,
        "camera": film_camera,
        "bloom": film_bloom,
        "vignette": { "type": "number", "minimum": 0, "maximum": 1, "default": 0, "description": "Corner darkening strength." },
        "white_balance": film_white_balance,
        "device": { "type": "string", "enum": ["auto", "gpu", "cpu"], "default": "auto", "description": "Where to trace. auto: GPU when available and within device limits, CPU fallback otherwise. gpu: also tries again after the GPU stopped responding earlier (auto then stays on the CPU). HDR finishing is shared. The reply's device {used: gpu|cpu, gpu: adapter name, fallback_reason: why not the GPU (null when it traced or cpu was asked for), asked, mixed: shutter slices traced on different devices, warning: a known GPU difference} says what traced; each view lists its own; asking for gpu and getting the CPU also adds a warnings line." },
        "tonemap": { "type": "string", "enum": ["aces", "agx", "filmic"], "default": "aces", "description": "ACES: default contrast; AgX: smooth highlight gamut compression; filmic: hue-preserving shoulder for saturated glowing eyes." },
    });
    film_schema["properties"].as_object_mut().unwrap().extend(film_lighting.as_object().unwrap().clone());

    film_schema["properties"].as_object_mut().unwrap().extend(json!({
        "look":{"type":"string","enum":["neutral","trailer","modern_warfare","desert_heat","night_ops","cinematic_teal_orange"],"description":"One-call camera, lighting, grade, atmosphere and finishing preset. Explicit options override preset fields recursively; false/null disables a preset effect."},
        "grade":{"type":"object","properties":{
            "look":{"type":"string","enum":["neutral","modern_warfare","desert_heat","night_ops","cinematic_teal_orange"]},
            "lift":vec3("Display-linear RGB offset, each -0.5..0.5, default 0."),
            "gamma":vec3("RGB gamma, each 0.1..5, default 1."),
            "gain":vec3("RGB multiplier, each 0..5, default 1."),
            "saturation":{"type":"number","minimum":0,"maximum":2},
            "contrast":{"type":"number","minimum":0.1,"maximum":3,"description":"Contrast around linear 18% grey; default 1 (preset may override)."}
        },"additionalProperties":false},
        "grain":{"type":"number","minimum":0,"maximum":0.15,"description":"Deterministic luminance-dependent film grain, default off; subtle 0.008..0.015."},
        "grain_seed":{"type":"integer","minimum":0,"maximum":16777215,"default":1},
        "sharpen":{"type":"number","minimum":0,"maximum":1,"description":"Locally range-limited unsharp mask after tone and grade, before grain; avoids edge ringing. Default off."},
        "motion_blur":{"description":"false (default), true (8 shutter strata over frame +/-0.25), or an object. Requires frame. Evaluates object/rig/camera motion at each stratum; averages scene-linear HDR before final post. Small step counts can show strobing; up to 16.","oneOf":[{"type":"boolean"},{"type":"object","properties":{
            "open":{"type":"number","minimum":-10,"maximum":10,"description":"Shutter opening offset from frame, in animation frames, default -0.25."},
            "close":{"type":"number","minimum":-10,"maximum":10,"description":"Closing offset, must exceed open; default 0.25."},
            "steps":{"type":"integer","minimum":2,"maximum":16,"default":8},
            "camera_end_position":vec3("World position at shutter close; linearly interpolated from camera position at shutter open."),
            "camera_end_target":vec3("World look-at point at shutter close; interpolated independently.")
        },"additionalProperties":false}]}
    }).as_object().unwrap().clone());
    film_schema["properties"]["fog"]["description"] = json!("Height fog/dust with single scattering, shadowed sun/light god rays and aerial perspective, CPU/GPU alike; composes with existing fire/smoke volume attenuation. true, a density, false/off, or {density: per meter (0..50), color: scattering albedo RGB (0..1), height: dense below this y, falloff: exponential height scale in meters (0 uniform), anisotropy: HG -0.95..0.95, distance: max medium path in meters, noise: heterogeneous 3D variation 0..1 (default 0), noise_scale: inverse meters 0.0001..100 (default 0.5), seed: integer 0..16777215}. Noise integrates with bounded 24..96 step quadrature; keep noise_scale * distance modest. Dust: {density:0.025,height:0.5,falloff:2.5,noise:0.7,noise_scale:0.35,anisotropy:0.65}. God rays require illumination and occluders.");

    json!([
        tool("fire", "Create or re-simulate target Fire as an atomic cancellable background job. Presets torch, candle, campfire, brazier, bonfire, burning_debris, smoke_column, embers. Settings: resolution 12..128, radius meters, fuel, smoke, intensity, light_gain (artist scene-light multiplier), temperature remap, wind, seed, fps, start, frames 1..96, warmup seconds, points (sphere emitter centers in local space). Maximum 256 MiB cached cells (64 cells supports 32 frames; 128 supports four). translation/parent place emitter; emitter samples up to 256 vertices from a mesh in its local space relative to translation and parents fire to it. Cache is deterministic, saved with scene, clamped outside range. Film CPU/GPU trace blackbody volume emission and HG smoke, smoke shadows, four emission-weighted local lights. Viewport uses cheap alpha-cut cross cards. GLB embeds a two-view, up-to-16-frame RGBA/emissive atlas with soft BLEND cross cards and extras.maya_fire describing engine UV animation; glTF does not natively animate texture UVs. Old kit mesh flames: kit lit:false, light:false, then parent fire to prop at the wick/bowl.", json!({"type":"object","properties":{
            "preset":{"type":"string","enum":["torch","candle","campfire","brazier","bonfire","burning_debris","smoke_column","embers"]},"name":{"type":"string"},"target":{},"parent":{},"emitter":{},"translation":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"settings":{"type":"object"}
        }})),
        tool("cloth", "Create and drape a sewn Cloth mesh on parent clay, or re-drape target cloth. Coordinates and lengths are parent rest-space meters. Patterns: tube (bone or center), skirt (waist band), shirt/tunic (fitted shoulders, round/v/boat neck, sleeves:true sews continuous sleeves to wrists), cape (two clasps), panel (simple polygon points, pins are corner indices). Defaults size from body/rig. fabric cotton/linen/wool/silk/leather/denim controls XPBD stretch/shear/bend, density and friction. Belt is local Y of a cinched pinned waist band. resolution 8–96; higher resolves smaller folds. thickness is full shell thickness with rounded hems. A mid_thigh tunic with sleeves:true and a belt pin is a complete garment; retain a visible leather belt. Parent belt/buckle blobs are excluded from belt-pinned garment contact so fabric sits underneath. Runs as an atomic cancellable job. Pass frames and clip to simulate motion; cache saves with scene, viewport and film interpolate it. Sleeved shirt/tunic caches skin the upper garment and simulate the free hem below belt/hips, avoiding bent-elbow contact ambiguity. Exports preserve the drape and parent skin; cached animation exports use vertex translation joints (larger files). bake preserves pattern UVs and caps triangles; lower resolution if seams block budget.", json!({
            "type":"object", "properties": {
                "parent": {"description":"Parent clay id or name (required for a new garment)"}, "target": {"description":"Existing Cloth id or name"},
                "name":{"type":"string"}, "color":vec3("Garment tint"), "material":{"type":"string"},
                "pattern":{"type":"string","enum":["tube","skirt","shirt","tunic","cape","panel"]},
                "length":{"description":"Meters or waist/hip/mid_thigh/knee/ankle"}, "fit":{"type":"number","default":0.02},
                "flare":{"type":"number","default":0.3}, "gather":{"type":"number","default":0.18,"description":"Extra waist circumference gathered into skirt pleats (0–0.75)"}, "resolution":{"type":"integer","minimum":8,"maximum":96,"default":56},
                "thickness":{"type":"number","default":0.002}, "fabric":{"type":"string","enum":["cotton","linen","wool","silk","leather","denim"]},
                "bone":{"type":"string"}, "center":vec3("Pattern origin in parent rest coordinates"), "radius":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}},
                "points":{"type":"array","items":vec3("Panel polygon corner")}, "pins":{"type":"array","items":{"type":"integer"}},
                "sleeves":{"type":"boolean","default":false}, "subdivision":{"type":"integer","minimum":0,"maximum":2,"default":1,"description":"Render-only Loop subdivision before thickness and rounded rims; bake folds to a game budget"},
                "belt":{"type":"number"}, "neckline":{"type":"string","enum":["round","v","boat"]}, "seconds":{"type":"number","default":2},
                "frames":{"type":"array","items":{"type":"number"}}, "clip":{"type":"string"},"fps":{"type":"number"}
            },"additionalProperties":false
        })),
        tool("get_scene", "Compact scene counts, top-level objects, selection and camera. all:true includes children; parent, target and filter narrow the list; offset/limit paginate. detail:true or verbose:true returns full object properties. \
            ui_activity lists the human's latest brush strokes (newest last): tool, target, brush, radius, samples, job, status, step and record_ids; with detail:true each also has request, the exact tool request the editor sent (send it to that tool to repeat the stroke).", empty.clone()),
        tool("create_object", "Add a primitive, an empty Group, a Clay sculpture (pass blobs), or an Eye. Primitives fit a 1x1x1 box (Sphere: diameter 1). Solids default to sitting on the grid at the origin; Clay and Eye start at the origin. \
            Eye: a ready-made eyeball of diameter 1 (size it with scale, e.g. 0.024 for a human), looking down its local +Z, with a sharp iris and pupil, a cornea bulge, and glossy materials; \
            much crisper than painting eyes on clay. Usually: parent the head clay, bone the head bone, mirror true, aim a point in front of the face. \
            Hair: hair cards, the way games do hair: strips with a strand texture with see-through gaps, grown along guide curves (see hair). Parent it to the head and bone it to the head bone. \
            color tints the whole object: on Clay and Eye it multiplies the blob, paint and eye colors (keep it [1, 1, 1] there). \
            For a finished look, give surfaces a material (skin, cloth, leather, hair, metal...: see material) and, for games, bake the clay (see bake). \
            Values are local to the parent when one is given. The new object becomes selected.", json!({
            "type": "object",
            "properties": { "shape": shape, "name": { "type": "string" }, "material": material.clone(), "eye": eye.clone(), "hair": hair.clone(), "aim": aim.clone(), "kit": kit_settings.clone(), "terrain": terrain_settings.clone(), "light": light.clone(),
                "mirror": { "type": "boolean", "description": "Also make a mirrored copy across the parent's X = 0 (other eye, other ear): position and rotation flipped, .L/.R or _L/_R swapped in the name and bone. Returned as mirror." }, "parent": target.clone(), "bone": { "type": ["string", "null"], "description": "Follow this bone of the parent's rig (parent must be rigged clay), e.g. a prop in a hand. translate/rotate/scale are then in the parent's space at the rest pose." }, "translate": vec3("Position."), "rotate": vec3("Rotation in degrees."), "scale": vec3("Scale."), "color": color.clone(),
                "blobs": { "type": "array", "items": blob.clone(), "description": "Clay only: the starting blobs." },
                "resolution": { "type": "integer", "minimum": 16, "maximum": 512, "description": "Clay only: grid cells across the longest side (default 96). Higher = finer detail (and finer sculpt and paint, which take the cell size of the first stroke), slower. A 1.75 m body at 512 has 3.4 mm cells: enough for fingers and lips." } },
            "required": ["shape"],
        })),
        tool("update_object", "Change any properties of one object. translate/rotate/scale are local (relative to the parent) unless space is world. Omitted properties stay the same. \
            Posing props without hand-made axes: align {two of x_axis, y_axis, z_axis, forward (+Z), up (+Y): world directions} turns the object about its pivot so those axes point there (the first given, or primary, exactly; the second as close as it can). \
            frame {parent: an object or a bone name, offset, rotate, roll, at, point, attach} puts the object in that frame: its pivot (or point: [x,y,z] in its own space, or a blob name) at the frame's origin plus offset, its axes the frame's turned by roll (degrees about the frame's Y) then rotate. \
            A bone's frame (rest pose): origin at (0 head .. 1 tail, default 0), +Y along the bone, +Z toward the rig's front as far as the bone allows, +X = Y x Z; an object's frame: its pivot and axes. attach true also hangs it on that bone. \
            grip {bone, point, roll, rotate, offset, at (default 0.5)} is frame with attach: a prop held by a named point (a handle blob) in a hand bone with a given roll. Replies give world_axes_at_rest. \
            Moving an object takes the kitbash parts on it along; moving a kitbash group keeps it where you put it relative to its object.", json!({
            "type": "object",
            "properties": { "target": target, "align": { "type": "object", "description": "Two world directions for two of the object's axes: x_axis, y_axis, z_axis, forward (= z), up (= y); primary names the exact one." }, "frame": { "type": "object", "description": "{parent (object or bone), offset [x,y,z], rotate [deg], roll (deg about the frame's Y), at (0..1 along a bone), point ([x,y,z] own space or blob name), attach (ride that bone)}" }, "grip": { "type": "object", "description": "{bone, point ([x,y,z] or blob name on the prop), roll, rotate, offset, at (default 0.5)}: hold the prop in a hand bone (it rides the bone)." }, "space": { "type": "string", "enum": ["local", "world"], "description": "world: translate/rotate/scale give where the object should be seen now, in the current pose (to put a prop in a posed hand)." }, "name": { "type": "string" }, "bone": { "type": ["string", "null"], "description": "Follow this bone of the parent's rig (parent must be rigged clay), e.g. a prop in a hand. translate/rotate/scale are then in the parent's space at the rest pose." }, "translate": vec3("Position."), "rotate": vec3("Rotation in degrees."), "scale": vec3("Scale."), "color": color, "visible": { "type": "boolean" }, "material": material, "eye": eye, "hair": hair, "aim": aim, "kit": kit_settings, "terrain": terrain_settings, "light": light.clone() },
            "required": ["target"],
        })),
        tool("mirror_object", "Mirror an object across a plane (default world x = 0): a real mirror copy, not a negative scale. Its geometry is reflected (clay blobs flip; sculpt, paint, masks and decals mirror; symmetric primitives and eyes just turn), it keeps a pivot of its own (the reflected one), .L/.R (_L/_R, Left/Right) swap in names, bones it rides, blob bones and rig bones, materials and colors carry over, and its children and the kitbash parts on it (and on its children) are mirrored with it onto the other side's bones. \
            Works on clay, primitives, kit pieces (which mirror by matrix), parts, groups and objects with negative or non-uniform scale (a part mirrored once by negative scale comes back with a normal one). \
            linked: true keeps the copy the reflection of its source: later blob edits, sculpt, paint, materials, moves, align and new detail passes on the source show on the copy until action unlink. replace: true first deletes an existing object with the copy's name (and the kitbash parts on it), to redo the other side. \
            plane: x | y | z (with at, default 0) or {normal, point}; plane_x is a shorthand. name names the copy.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "plane": { "description": "x, y, z or {normal [x,y,z], point [x,y,z]} (world).", "oneOf": [{ "type": "string", "enum": ["x", "y", "z"] }, { "type": "object" }] },
                "at": { "type": "number", "description": "Offset of an x/y/z plane (default 0)." },
                "plane_x": { "type": "number" },
                "linked": { "type": "boolean", "description": "Keep following the source's edits (default false)." },
                "replace": { "type": "boolean", "description": "Delete an existing object with the copy's name first (with its kitbash parts)." },
                "kitbash": { "type": "boolean", "description": "Mirror the kitbash parts on it too (default true)." },
                "name": { "type": "string" },
                "action": { "type": "string", "enum": ["mirror", "unlink"], "description": "unlink: a linked copy stops following its source." },
            },
            "required": ["target"],
        })),
        tool("pose_bone", "Pose in place: move or turn a bone's REST pose (and the bones under it) and carry everything that rides it: props and armor on those bones, the rig clay's blobs assigned to them (plus blobs by name), their weight strokes, and kitbash parts. Clips keep their keys (they are relative to the rest pose). \
            rotate [degrees XYZ] turns about the bone head (or pivot), in world axes (space local: the rig's axes); translate moves; head / tail give new rest positions; aim points the tail at a world point. mirror: true does the mirrored move on the other side's bone. carry false moves only the bones. Reply: what moved and the new rest joints.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "bone": { "type": "string" },
                "rotate": vec3("Degrees XYZ, about the bone head (or pivot)."),
                "translate": vec3("Move."),
                "pivot": vec3("Turn about this point instead of the head."),
                "head": vec3("New rest head (moves the bone)."),
                "tail": vec3("New rest tail (turns the bone about its head)."),
                "aim": vec3("Turn so the tail points at this point."),
                "space": { "type": "string", "enum": ["world", "local"] },
                "carry": { "type": "boolean", "description": "Carry what rides the bones (default true)." },
                "blobs": { "type": "array", "items": { "type": "string" }, "description": "More rig clay blobs (by name) to carry." },
                "mirror": { "type": "boolean", "description": "Also move the other side's bone the mirrored way." },
            },
            "required": ["bone"],
        })),
        tool("damage", "Apply physical battle damage and/or a wear preset atomically to target or targets (Clay, Kit or primitives). One preset plus a handful of stamps finishes an asset. Existing paint and stencils remain under the wear. Presets update shared materials (all users keep receiving base edits; no per-object copies). World-space point stamps select the nearest target; gouge/scrape paths select their start target. Presets: military_paint (matte microtexture), battle_worn (layered light paint chips, primer, bare steel, cavity grime), desert_dusty (world-up dust/sun fade), muddy (world-height accumulation), burnt_wreck, factory_new (removes procedural wear). amount 0..1; reapplying a preset replaces its amount. clear removes damage stamps, preserving old decals. Stamps combine real geometry displacement with per-pixel color/roughness/metal and normal detail, carried by film CPU/GPU, clay bake and GLB. World rest-pose coordinates by default; space local for object coordinates. Each stamp has kind, point or points, optional normal, size (full halo width, meters), depth (meters), seed. bullet_hit caliber_mm 5.56..25 defaults size to 4x caliber. dent chips bent paint; scorch is soft soot/char with restrained heat tint and optional melted_lip:true; blast adds crater and shrapnel pits. gouge/scrape points draw grooves with curled edges and parallel fine scratches. rust_streak/oil_leak/water_stain integrate world gravity over the mesh, length defaults .25m; choose a sloping/vertical start. bend/tear support hinge:[[x,y,z],[x,y,z]] and angle degrees. Random count requires region:{center,radii}; sampling is deterministic by seed. Maximum 64 saved decals/damage segments per object. Damage locally refines clay/kit triangles to resolve caliber-sized hits without globally increasing clay resolution. Refinement is bounded to 200k additional triangles per mesh; excessive batches may need a finer base mesh.", json!({
            "type":"object", "properties": {
                "target":target.clone(),"targets":{"type":"array","items":target.clone()},
                "preset":{"type":"string","enum":["military_paint","battle_worn","desert_dusty","muddy","burnt_wreck","factory_new"]},
                "amount":{"type":"number","minimum":0,"maximum":1,"default":0.65},
                "space":{"type":"string","enum":["world","local"],"default":"world"},"clear":{"type":"boolean"},
                "stamps":{"type":"array","maxItems":64,"items":{"type":"object","properties":{
                    "kind":{"type":"string","enum":["bullet_hit","dent","scorch","blast","gouge","scrape","rust_streak","oil_leak","water_stain","bend","tear"]},
                    "point":vec3("Surface start"),"points":{"type":"array","items":vec3("Surface point"),"maxItems":64},"normal":vec3("Outward normal"),
                    "size":{"type":"number"},"depth":{"type":"number"},"melted_lip":{"type":"boolean"},"caliber_mm":{"type":"number","minimum":5.56,"maximum":25},
                    "width":{"type":"number","description":"Alias for size; runoff halo width in meters."},
                    "color":vec3("Optional stain color, sRGB"),"softness":{"type":"number","minimum":0.1,"maximum":2,"default":1},
                    "impact_angle":{"type":"number","minimum":0,"maximum":80,"default":0,"description":"Bullet obliquity from surface normal."},"rotate":{"type":"number"},
                    "length":{"type":"number"},"seed":{"type":"integer","minimum":0},"count":{"type":"integer","minimum":1,"maximum":64},
                    "region":{"type":"object","properties":{"center":vec3("Region center"),"radii":vec3("Region radii")},"required":["center","radii"]},
                    "hinge":{"type":"array","items":vec3("Hinge endpoint"),"minItems":2,"maxItems":2},"angle":{"type":"number","minimum":-85,"maximum":85}
                },"required":["kind"],"additionalProperties":false}}
            }
        })),
        tool("wear", "Apply one wear preset to target or targets. Same arguments as damage; preset and amount are sufficient. Updates shared materials and reports this in the reply; all users of those materials receive the wear. World-aware masks follow rotated and bone-attached parts; factory_new removes added wear layers.", json!({"type":"object","properties":{"target":target.clone(),"targets":{"type":"array","items":target.clone()},"preset":{"type":"string","enum":["military_paint","battle_worn","desert_dusty","muddy","burnt_wreck","factory_new"]},"amount":{"type":"number","minimum":0,"maximum":1}},"required":["preset"]})),
        tool("set_material", "Define or change a scene material (overrides a preset of the same name), or remove one. Without name, only lists. \
            Smart presets are whole layered looks under one name (worn_iron, gold_trim, old_leather, demon_skin, dungeon_stone...: use the name as a blob or object material directly); \
            to tune one, set_material with its name and wear / rust / dirt, or base a new one on it (based_on) and add_layers. \
            Recipes: plate armor = worn_iron, with gold_trim on the rim blobs and old_leather straps; a rusty dungeon gate = rusted_iron; a demon = demon_skin with wet_flesh in the mouth and charred_flesh wounds; \
            a bloody blade = {name: bloody_steel, layers: [{smart: polished_steel}, {smart: blood_splatter}]}; walls = dungeon_stone, mossy_stone low down; a peasant = grimy_cloth tunic and old_leather boots. \
            Materials set how light reflects; colors still come from blobs, paint and object color. lips is soft, slightly moist and subsurface: use rose color [0.70,0.43,0.42] on its blob/object. Paint the whole lip surface continuously through the meeting line; two separate narrow strokes leave a pale seam. On an existing human, a small cover ellipsoid enclosing both lips with offset 0.00002, blend 0, material lips and a base close to skin follows the shape; erase old lip paint, then paint a soft rose gradient across both lips and the meeting line. Exports carry them (glTF: metallic/roughness, clearcoat and sheen extensions; FBX: shininess and reflection). \
            Edits return the changed field names, layer count, user count and library count. Calls without name list materials and presets.", json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "base_color": {"type":["array","null"],"items":{"type":"number","minimum":0,"maximum":1},"minItems":3,"maxItems":3,"description":"Substrate sRGB color before painted markings; null inherits a named preset, otherwise uses blob/object color."},
                "keep_paint": {"type":"boolean","default":true,"description":"Preserve painted flags/stencils above base_color and below wear."},
                "based_on": { "type": "string", "description": "Start from this material's values (default: the current one of this name, else clay)." },
                "roughness": { "type": "number", "minimum": 0, "maximum": 1, "description": "0 = mirror-sharp highlights, 1 = matte." },
                "metallic": { "type": "number", "minimum": 0, "maximum": 1, "description": "1 = bare metal: reflections take the surface color." },
                "clearcoat": { "type": "number", "minimum": 0, "maximum": 1, "description": "A thin glossy top layer: wet, varnished, lacquered." },
                "sheen": { "type": "number", "minimum": 0, "maximum": 1, "description": "Soft light at grazing angles: velvet, cloth fuzz, peach-fuzz skin." },
                "subsurface": { "type": "number", "minimum": 0, "maximum": 1, "description": "Light that spreads under the surface and glows back out: skin (the skin preset uses 0.8), wax, marble, leaves. Seen in render_film." },
                "pores": { "type": "number", "minimum": 0, "maximum": 1, "description": "Skin micro bump and roughness variation in film and bake, at 0.25 mm spacing in object meters. Requires subsurface; skin preset 0.65, 0 disables. Bake texture resolution must resolve the detail." },
                "skin_specular": { "type": "number", "minimum": 0, "maximum": 1, "description": "Film: blend tight and broad GGX skin highlights (skin/lips presets 0.8). 0 uses one lobe. Color still comes from the surface." },
                "mottling": { "type": "number", "minimum": 0, "maximum": 1, "description": "Subtle pigment variation in film and baked color, on subsurface materials; skin preset 0.4, 0 disables." },
                "emission": { "type": "number", "minimum": 0, "maximum": 100, "description": "Light the surface gives off, in its own color (blob/paint/object color): 1 = as bright as a white surface in the key light; the glow preset uses 2.5 (higher washes the color toward white). Glowing eyes, lava, runes, screens. Lights nearby surfaces in render_film; exports as emissive." },
                "transmission": { "type": "number", "minimum": 0, "maximum": 1, "description": "How much light passes through, bending at the surface: 1 = clear glass (the glass, water, crystal presets). The surface color tints it by hue (white or gray = clear). roughness frosts it. Make glass objects closed shells (a thin glass needs an inner and outer surface; use a carve inside). Shadow rays pass glass. Exports as KHR_materials_transmission." },
                "cavity": { "type": "number", "minimum": 0, "maximum": 1, "description": "Weathering: darken creases and the gaps between scales, folds, seams (found from the shape). 0.5-0.8 gives depth to scales, skin folds, armor." },
                "edge_wear": { "type": "number", "minimum": 0, "maximum": 1, "description": "Weathering: rub ridges and edges lighter and smoother (worn leather, scale rims, armor edges)." },
                "dirt": { "type": "number", "minimum": 0, "maximum": 1, "description": "Weathering: grime in creases and on down-facing parts, patchy; color dirt_color (default dark brown)." },
                "dirt_color": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "sRGB color of the dirt." },
                "variation": { "type": "number", "minimum": 0, "maximum": 1, "description": "Weathering: color blotches (brightness and a little hue) so a surface is never one flat color. 0.3-0.6 for skin, scales, cloth." },
                "gloss_variation": { "type": "number", "minimum": 0, "maximum": 1, "description": "Weathering: patchy shine (some areas duller, some glossier). Breaks the plastic look." },
                "variation_scale": { "type": "number", "minimum": 0, "description": "Size of the blotches in scene units (default: from the object's size)." },
                "ior": { "type": "number", "minimum": 1, "maximum": 3, "description": "How strongly see-through materials bend light: 1.33 water, 1.5 glass (default), 2.0 crystal, 2.42 diamond." },
                "scatter": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "How far red, green, blue light travel under the surface, in scene units (default skin: [0.0037, 0.0014, 0.0007] for a 1.7-unit person; scale with the character)." },
                "detail": { "type": ["string", "null"], "description": "Real surface texture from the built-in CC0 library (scanned PBR maps): linen, jersey, knit, felt, velvet, denim (cloth); leather_smooth, leather_rough, leather_grain, suede; skin_pores, wrinkles; metal_brushed, metal_scratched; wood, stone, scales. Projected from the three axes (no UVs needed); bends the normal, varies roughness and color. Shown in the viewport and render_film, and baked into exported textures. Presets come with one (cloth: linen, leather: leather_rough, skin: skin_pores, lips: wrinkles, velvet, wood, metal: metal_brushed, scales); a scene material named like a preset keeps the preset's unless it sets its own. \"none\" or null = off." },
                "detail_scale": { "type": "number", "minimum": 0, "maximum": 100, "description": "Size of one texture tile in scene units (default 0 = its real size: linen 12 cm, knit 12 cm, wood 60 cm, pores 5 cm). Smaller = finer." },
                "detail_strength": { "type": "number", "minimum": 0, "maximum": 4, "description": "How deep the relief looks (normal and roughness), default 1. Skin preset 0.6." },
                "detail_color": { "type": "number", "minimum": 0, "maximum": 1, "description": "How much the texture's own light and dark pattern (threads, grain, rings) shows in the color, default 0.35; 0 = relief only. The hue stays the surface color." },
                "detail_rotate": { "type": "number", "description": "Turn the texture, in degrees (weave or grain direction)." },
                "detail_displace": { "type": "number", "minimum": 0, "maximum": 1, "description": "render_film only: move the surface by the detail map's height, this many scene units from lowest to highest, as real geometry (chipped stone, scale edges and bark stand out of silhouettes and cast their own shadows). The mesh is diced to about a pixel where the cameras see it, so close-ups cost more triangles (reported as displaced_triangles). 0 (default) = shading relief only. Stone walls 0.005-0.02, bark 0.005, dragon scales 0.003. Viewport and bakes keep the relief in the normals." },
                "layers": { "type": "array", "maxItems": 16, "description": "Smart material layers (like Substance Painter smart materials), bottom first, replacing the current ones. Each layer is a look laid over the ones below where its mask says: \
                    {name, color [r,g,b] (omit to keep the color below), tint [r,g,b] (multiplies), roughness, metallic, clearcoat, sheen, emission (glow), variation (0-1 color blotches, variation_scale m), detail / detail_scale / detail_strength / detail_color / detail_rotate (a library map for this layer), \
                    mask: [terms] (empty = everywhere), threshold and contrast (levels on the whole mask: threshold = where it flips, contrast = how sharp, e.g. 0.45 and 5), invert, opacity (0-1), role: wear | rust | dirt (follows the material's wear / rust / dirt amounts)}. \
                    Or {smart: \"preset\", opacity} splices a smart preset's layers in (e.g. [{smart: worn_iron}, {smart: blood_splatter, opacity: 0.6}]). \
                    A mask term is a source name or {source, op, invert, threshold, contrast, amount, ...}: edges (convex: worn polished edges), cavity (concave creases: dirt, rust, soot), ao (hidden places), up / down (which way the surface faces: dust and moss on top, drips under), \
                    direction {direction [x,y,z]}, height {range [y at 0, y at 1]} (object y: water line, mud on boots: [0.4, 0.05]), noise {scale m, octaves 1-6, seed}, grunge {map: library map name, scale m: its height as a ragged pattern; metal_scratched, metal_brushed and stone make good grunge, wrinkles and scales give crack lines (invert)}, \
                    painted {channel: name painted with paint channel}, constant {value}. Terms fold left to right with op: multiply (default), max, min, add, subtract, screen. Masks use object space. \
                    Layers show in the viewport (per vertex), render_film (per pixel) and bakes (per texel; glow is film only).", "items": { "type": "object" } },
                "add_layers": { "type": "array", "description": "Layers to put on top of the current ones (same form as layers), e.g. [{smart: blood_splatter}] or a moss layer.", "items": { "type": "object" } },
                "wear": { "type": ["number", "null"], "minimum": 0, "maximum": 1, "description": "Smart materials: how much of the wear layers (worn edges) show; default 0.5 (each preset's own), 0 = none, 1 = worn everywhere it can be." },
                "rust": { "type": ["number", "null"], "minimum": 0, "maximum": 1, "description": "Smart materials: how much of the rust layers (rust, verdigris, moss) show; default 0.5. dirt does the same for grime, soot, stains and mud layers (and is weathering dirt on materials without layers)." },
                "remove": { "type": "boolean" },
            },
        })),
        tool("bake", "Cloth keeps pattern UVs and bakes fine folds/weave; Make a Clay object game-ready, the way game studios do: simplify it to a triangle budget, lay it flat (UV unwrap), and bake the exact clay surface into textures: \
            color (blobs + paint), a normal map (sculpted bumps and smooth curves the simple mesh lacks), and AO/roughness/metallic (shadow in creases, and the materials). \
            Skin weights carry over, so rigs and clips still work. The settings are saved on the object; from then on export_glb / export_fbx write the baked mesh with its textures (re-baking after edits). \
            The bake casts short rays at the clay's fine mesh (detail_cell and detail regions; the one render_film built, else the rest mesh refined where there is sculpt or paint), with ambient occlusion from the clay, and fills every gap between UV islands from the nearest island so mipmaps never bleed seams. A 4K bake of a whole sculpted body takes about 15-30 s (8K about 30-40 s); exact: true reads every texel from the clay field instead (slower, for comparisons). Detail regions get more of the triangle budget and more texels (region_density). Returns the counts and an image of the three textures. See the result with render_views baked: true. off: true goes back to exporting the clay mesh.             For lower levels of detail pass lods: true to export_glb / export_fbx.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "triangles": { "type": "integer", "minimum": 500, "maximum": 300000, "description": "Triangle budget (default 30000; game characters use about 20000 to 80000)." },
                "texture": { "type": "integer", "enum": [512, 1024, 2048, 4096, 8192], "description": "Texture size in pixels (default 2048). 4096 for a game character's body; 8192 for hero assets (each map is 256 MB in memory)." },
                "region_density": { "type": "number", "minimum": 0, "maximum": 1, "description": "How much more texture the clay's detail regions (face, hands) get: 1 = texels shrink with the region's cell (a 4x finer cell gets 4x the texels across), 0 = even. Default 0.5 (a 0.25 mm face on a 1 mm body gets 2x the texels across)." },
                "exact": { "type": "boolean", "description": "Read every texel from the exact clay field (slow, the old way) instead of the fine mesh." },
                "show": { "type": "boolean", "description": "Also show baked meshes in the viewport (like set_display baked)." },
                "off": { "type": "boolean" },
                "maps": { "type": "array", "maxItems": 2, "items": { "type": "string", "enum": ["displacement", "vector_displacement"] }, "description": "Also write displacement maps over the bake's UVs, for renderers and sculpting apps that displace the simple mesh back into the clay: displacement (height along the simple mesh's normal) and/or vector_displacement (the offset in its tangent frame: R along +u, G along the bitangent, B along the normal). The bake finds the clay's surface only along the simple mesh's interpolated normal from each texel, so R and G are about 0 and B is the height map's value: the vector map is that offset in vector form, for tools that read vector maps; it does not capture overhangs or detail pushed sideways. Files in save_dir named <object>_displacement / <object>_vector_displacement; one-off outputs, not saved on the object. The reply's maps says the files, scale and how to decode." },
                "map_size": { "type": "integer", "enum": [512, 1024, 2048, 4096], "description": "Displacement map pixels across (default the texture size, at most 2048)." },
                "map_format": { "type": "string", "enum": ["png", "exr"], "description": "png (default): 16-bit, value = (code / 65535 - 0.5) * scale, mid grey = 0, each map's scale in the reply; exr: 32-bit floats as they are (clay units)." },
                "save_dir": { "type": "string", "description": "Absolute folder for the maps (made when the maps are written, if missing); required with maps." },
                "verify": { "type": "boolean", "description": "With maps: reapply them as geometry (diced to half a cell) and report the error to the clay in cells (max_error_cells, mean_error_cells)." },
                "lowpoly": { "type": "string", "enum": ["meshopt"], "description": "How the simple mesh is made: meshopt (default; retopo arrives with the Stage 9 Poly track)." },
            },
            "required": ["target"],
        })),
        tool("surface_point", "Works in the rest pose: points in and out are rest-pose points, even while a clip plays. Find exact points on surfaces, to place eyes, hair roots, props, and paint or sculpt strokes. Three ways, any mix: \
            view + at (look straight in from front/back/right/left/top/bottom at screen points: front and back take [x, y], right and left [z, y], top and bottom [x, z]; e.g. view front at [[0.03, 1.6]] gives the face point there); \
            rays ([{origin, direction}], world space); points (world points snapped to the nearest surface of the target clay). \
            Returns each hit's object, point and normal. With target, only that object (and what is under it) is hit.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "view": { "type": "string", "enum": ["front", "back", "right", "left", "top", "bottom"] },
                "at": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 } },
                "rays": { "type": "array", "items": { "type": "object", "properties": { "origin": vec3("Start."), "direction": vec3("Direction.") }, "required": ["origin", "direction"] } },
                "points": { "type": "array", "items": vec3("A world point near the surface.") },
            },
        })),
        tool("check_export", "Load an exported GLB from disk and render that file with the film path tracer. Returns the picture and file geometry, materials/textures, images, skins, animations, bounds and warnings. Independent of the editor scene. Runs as a background job; after 40 s use job_status to fetch the answer. Supports embedded PNG/JPEG, triangle meshes, TRS/matrix node trees and LINEAR/STEP skeletal animation. device says which tracer ran (as in render_film).", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute .glb path to check." },
                "views": { "type": "array", "minItems": 1, "maxItems": 4, "items": view.clone(), "description": "Default [three_quarter]; framed on file bounds." },
                "size": { "type": "integer", "minimum": 64, "maximum": 1600, "description": "Pixels per view side (default 512)." },
                "samples": { "type": "integer", "minimum": 1, "maximum": 4096, "description": "Samples per pixel (default 32)." },
                "time_limit": { "type": "number", "minimum": 1, "maximum": 600, "description": "Seconds including loading and the ray tree (default 40)." },
                "animation": { "type": "string", "description": "Clip name; with no frame samples frame 0. No animation/frame shows the rest pose." },
                "frame": { "type": "number", "description": "Frame at file fps, with clip time zero as frame 0. Uses first clip if animation is omitted; older files infer fps from sample spacing (reported in warnings)." },
                "save_path": { "type": "string", "description": "Also save contact sheet to an absolute .png path." },
                "morph_weights": { "type": "object", "additionalProperties": { "type": "number" }, "description": "Morph target weights by name (glTF extras.targetNames), applied to the vertices; default the file's own weights." },
            },
            "required": ["path"],
        })),
        tool("render_film", "Film-quality picture from the path tracer (like Blender's Cycles): soft studio lights with real shadows, light bouncing between surfaces, glossy and metal materials, \
            baked textures and normal maps, see-through hair cards, a shadow-catching floor, denoised, filmic tone. Procedural skies or HDR image lighting, physical camera and polygonal thin-lens depth of field, motion blur, energy-conserving HDR glare, grading and film grain. look: trailer applies a photographic preset. Framed like render_views (views, center, radius). \
            Lights carried by objects (torches, candles, braziers, their scattered copies) light the scene as soft spheres; fog adds mist with light shafts; views [\"iso\"] and aspect 1.78 give a game-style environment shot (see kit). \
            Try golden_hour + lights none + dof f/2.8 focused on the eyes for portraits. Slow: seconds to minutes; samples and time_limit trade speed for less grain. Use render_views while working and this to judge the final look. \
            The reply's device field says whether the GPU or the CPU traced, and why not the GPU (e.g. a buffer over the GPU's limit, hair strands under auto).", film_schema),
        tool("create_human", "Start a character from a correct human base (MakeHuman's CC0 base mesh and shape targets, built in): \
            a body clay with a full rig (spine, neck, head, arms, three bones per finger, legs, feet), a finer head clay on the head bone, and Eye objects in the sockets. \
            Real anatomy, unlike blobs. Shape it with human (sex, age, weight, muscle, height, proportions, ancestry, and 600+ named face and body shapes; see human_controls), \
            then add clothes, hair and detail with the usual clay, sculpt, paint and material tools. Feet stand at y = 0, facing +Z.", json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Body object name (default Human); the head is NAME_head, eyes NAME_eye.L / .R." },
                "human": json!({
                "type": "object",
                "description": "Human controls. sex (0 female .. 1 male), age (18..90 years), weight, muscle, height, proportions (0..1; 0.5 average), african / asian / caucasian (ancestry mix, normalized), shapes {name: value} for any face or body target from human_controls, e.g. {\"nose/nose-scale-horiz\": -0.3, \"ears/ear-shape-pointed\": 1, \"chin/chin-prominent\": 0.4}.",
                "properties": {
                    "sex": { "type": "number" }, "age": { "type": "number" }, "weight": { "type": "number" }, "muscle": { "type": "number" },
                    "height": { "type": "number" }, "proportions": { "type": "number" }, "african": { "type": "number" }, "asian": { "type": "number" }, "caucasian": { "type": "number" },
                    "shapes": { "type": "object", "additionalProperties": { "type": "number" } },
                },
            }),
                "skin_color": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "sRGB skin color, default warm [0.82,0.64,0.54]. Independent of ancestry controls; choose it for the character." },
                "eye_color": color.clone(),
                "rig": { "type": "boolean", "description": "Add the rig (default true)." },
                "resolution": { "type": "integer", "minimum": 16, "maximum": 512, "description": "Body clay grid (default 384)." },
                "head_resolution": { "type": "integer", "minimum": 16, "maximum": 512, "description": "Head clay grid (default 320, about 1 mm)." },
                "head_detail_cell": { "type": "number", "description": "Fine sculpt/paint cell for the head (e.g. 0.0005 for pores and lines)." },
                "skin": skin.clone(),
                "face_hair": { "type": "boolean", "description": "Also grow brows, lashes and peach fuzz (face_hair with defaults)." },
                "detail": { "type": "object", "description": "Make the whole person ONE clay (head included, no separate head clay) with adaptive detail: {cell (everywhere, default 0.001), face (default 0.00025), hands (default 0.00025); null turns a region off}. The face and hands become detail regions placed from the skeleton (see edit_clay detail_regions); sculpt pores, wrinkles and knuckle creases there. Replies with the regions.", "properties": { "cell": { "type": "number" }, "face": { "type": ["number", "null"] }, "hands": { "type": ["number", "null"] } } },
                "one_object": { "type": "boolean", "description": "One clay for the whole person (implied by detail)." },
            },
        })),
        tool("set_human", "Change a human made by create_human (target: its body or head): human controls merge into the current ones (shapes merge by name; 0 removes one); skin settings (face pores, wrinkles, color, lips) merge the same way. The head, eyes and bone positions follow; clips are kept.", json!({
            "type": "object",
            "properties": { "target": target.clone(), "human": json!({
                "type": "object",
                "description": "Human controls. sex (0 female .. 1 male), age (18..90 years), weight, muscle, height, proportions (0..1; 0.5 average), african / asian / caucasian (ancestry mix, normalized), shapes {name: value} for any face or body target from human_controls, e.g. {\"nose/nose-scale-horiz\": -0.3, \"ears/ear-shape-pointed\": 1, \"chin/chin-prominent\": 0.4}.",
                "properties": {
                    "sex": { "type": "number" }, "age": { "type": "number" }, "weight": { "type": "number" }, "muscle": { "type": "number" },
                    "height": { "type": "number" }, "proportions": { "type": "number" }, "african": { "type": "number" }, "asian": { "type": "number" }, "caucasian": { "type": "number" },
                    "shapes": { "type": "object", "additionalProperties": { "type": "number" } },
                },
            }), "skin": skin.clone() },
            "required": ["target"],
        })),
        tool("face_detail", "Sculpts a human's face folds as real geometry on the head clay: nasolabial folds, forehead furrows, frown lines, crow's feet, under-eye creases, marionette and upper-lip lines, placed from the face's own landmarks and aged by its skin (set_human skin age / wrinkles). Uses a fine detail grid (detail_cell, default 0.00025 = 0.25 mm; takes a minute or two). Pores and the sharp bottoms of lines stay in shading. Running it again deepens the folds. Background job like sculpt.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "detail_cell": { "type": "number", "description": "Sculpt grid cell in meters (default 0.00025) when the head has no sculpt detail yet." },
                "strength": { "type": "number", "minimum": 0, "maximum": 3, "description": "Fold depth factor (default 1)." },
            },
            "required": ["target"],
        })),
        tool("face_hair", "Grows a human's facial hair from its face layout: brows (hairs in the real direction pattern: upright at the head, outward along the brow with rows meeting in the middle, the tail sweeping down; dense in the body, sparse at the edges), lashes (two staggered rows on the upper lid curling up and fanning out at the outer corner, a sparse lower row; from the lid margins) and peach fuzz (short, thin, pale vellus hair lying down and outward; it catches rim light). Each is a Hair object on the head (NAME_brows, NAME_lashes, NAME_fuzz), replaced when grown again. Film renders them as strands; exports get thin cards. Each part: true (default), false (remove), or {density (0..2, default 1), color [r,g,b]}.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "brows": { "description": "true, false or {density, color}; default dark brown." },
                "lashes": { "description": "true, false or {density, color}; default near black." },
                "fuzz": { "description": "true, false or {density, color}; default pale blond." },
                "seed": { "type": "integer" },
            },
            "required": ["target"],
        })),
        tool("human_controls", "List the human shape controls (600+ face and body targets) with their ranges. filter narrows by text, e.g. nose, ears, mouth, eyes, chin, cheek, head, torso, breast, armslegs.", json!({
            "type": "object",
            "properties": { "filter": { "type": "string" } },
        })),
        tool("job_status", "No call waits longer than about 40 s. Long work (render_film, bake, exports) runs in the background with a progress bar in the editor, and so does a call that waits for meshes to build. \
            When time is up, the call answers with a job id instead of its result, and a status: 'running' (a job), 'done, answer pending' (the change IS made; only its summary waits for meshes), or 'waiting in line (not run yet)' (it runs later, in order; do not send it again). \
            Call this with that id to get the result. wait: N (up to 45) answers as soon as the job is done, or after N seconds with how far it is (percent, seconds_left): use it instead of polling. \
            cancel: true stops a running job (nothing it made is kept) or drops a call still in line. Without id, lists pending and finished jobs and ui_activity (the human's latest strokes, as in get_scene). Results remain retrievable for 24 hours, including after timeouts and repeated reads.", json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "wait": { "type": "number", "minimum": 0, "maximum": 45, "description": "Seconds to wait for the job to finish before answering (default 0)." },
                "cancel": { "type": "boolean", "description": "Stop the job (running) or drop the call (waiting in line)." },
            },
        })),
        tool("delete_object", "Remove one object and everything under it.", json!({ "type": "object", "properties": { "target": target.clone() }, "required": ["target"] })),
        tool("duplicate_object", "Copy one object (and its children) in place. The copy becomes selected. Identical clay copies share one mesh (built once, drawn instanced) until one is edited.", json!({ "type": "object", "properties": { "target": target.clone() }, "required": ["target"] })),
        tool("instance_array", "Turn an array blob of a clay object into instances: one copy is meshed (a hidden clay under the object) and a scatter under the object holds every copy, which the viewport draws instanced and render_film and exports share. The blob leaves the clay. Refused when separate copies would not look the same: the blob is not an added, unmirrored array, other blobs blend with, carve into or cover the copies, or the clay has sculpt, paint, masks or decals (force: true instances anyway; copies are then separate closed shapes that do not merge with their neighbours). Reply: unit, scatter, copies, triangles before and after (drawn and unique).", json!({ "type": "object", "properties": {
            "target": target.clone(),
            "blob": blob_key.clone(),
            "level": { "type": "string", "enum": ["outer", "leaf"], "description": "Nested arrays: outer (default) copies the inner array as one unit; leaf copies the innermost shape." },
            "force": { "type": "boolean", "description": "Skip the neighbour, sculpt and paint checks (default false)." },
        }, "required": ["target", "blob"] })),
        tool("select_object", "Set the selection. Pass target and/or targets; omit both to clear. add: true adds to the current selection. The last one is the lead.", json!({ "type": "object", "properties": { "target": target.clone(), "targets": { "type": "array", "items": target.clone() }, "add": { "type": "boolean" } } })),
        tool("group_objects", "Put objects under a new empty group (Maya Ctrl+G), keeping where they are. Defaults to the selection.", json!({ "type": "object", "properties": { "targets": { "type": "array", "items": target.clone() }, "name": { "type": "string" } } })),
        tool("parent_objects", "Move children under a parent (or to the world when parent is null), keeping where they are.", json!({ "type": "object", "properties": { "children": { "type": "array", "items": target.clone() }, "parent": target.clone() }, "required": ["children"] })),
        tool("set_pivot", "Move an object's pivot (the point it rotates and scales around) without moving the object. Give a world-space pivot, or center: true for its bounding-box center.", json!({ "type": "object", "properties": { "target": target.clone(), "pivot": vec3("World-space point."), "center": { "type": "boolean" } }, "required": ["target"] })),
        tool("set_camera", "Move the orbit camera. Omitted values stay the same. preset iso gives the Diablo-style high three-quarter view (yaw 45, pitch 52) framing everything, before the other values apply.", json!({
            "type": "object",
            "properties": { "preset": { "type": "string", "enum": ["iso"] }, "look_at": vec3("Point the camera orbits."), "yaw": { "type": "number", "description": "Degrees around Y." }, "pitch": { "type": "number", "description": "Degrees up from the ground plane, -89..89." }, "distance": { "type": "number", "minimum": 0.1 } },
        })),
        tool("kit", "Build environments from a modular dungeon kit, one call per piece: real geometry, every stone its own chipped, rounded, slightly uneven block with its own tone over dark mortar, weathered (darker joints, worn edges, damp stains), so light catches it. \
            Pieces (meters; y = 0 is the floor): wall (along local X from -length/2 to length/2, default 2 x 3 m, 0.5 thick), corner (a quoin column on its grid point covering two wall ends; long stones toward +X and +Z, turn to fit), doorway (a wall with an opening: opening width 1.3, opening_height spring 2, arch round|pointed|flat, ring voussoir depth 0.3, keystone), pillar (kind round|square, width 0.6, height 3; plinth, drums, capital), stairs (width 2, steps 5, rise 0.18, run 0.34, climbing +Z), floor (length x depth from the origin, tops at y = 0; pattern flagstone|bricks|cobble, block stone size, gap, unevenness, damage cracks and missing stones), vault (barrel vault along Z, width span 4, springing at height 3), \
            rock (size [x,y,z], roughness, strata layering, facets broken faces), rubble (pile: width diameter 1.6, height 0.45, count 40 pieces), torch (kind wall: bracket at z = 0 sticking out +Z, or stand; flames and a torch light), candle (count 1..7 in a cluster, height 0.18; wax glows; a candle light), brazier (iron bowl, glowing coals, flames; a strong light), log (unit cylinder of charred wood with emissive crack texture; scale/rotate/place underneath campfire emitters; lit:false disables ember glow), bones (kind femur|skull|ribs|pile), chain (length, hanging down from the origin), sarcophagus (length 2.1 along X, width 0.9, height 0.85: plinth, panelled body, gabled lid). \
            Shared: seed (other stones), color (sRGB), damage 0..1, roughness 0..1, gap, block, course, detail (mesh density 0.25..3), lit false (props without flames). \
            Architecture snaps: at's X and Z to a 0.5 m grid, turn to quarter turns, lengths to whole meters, so walls and corners meet (snap: 0 turns it off, or another step). A wall at [0,0,-4] runs x -1..1; the next goes at [2,0,-4]; a corner at [-4,0,-4] covers both wall ends. \
            target changes an existing piece (settings merge). Materials: built-in stone, mortar, grout, iron, wood, wax, flame, ember, coal, bone; set_material with one of those names restyles every piece; material on the piece replaces its main one. Returns the object, settings, triangles and world bounds.", json!({
            "type": "object",
            "properties": {
                "piece": { "type": "string", "enum": ["wall", "corner", "doorway", "pillar", "stairs", "floor", "vault", "rock", "rubble", "torch", "candle", "brazier", "log", "bones", "chain", "sarcophagus"] },
                "target": target.clone(), "name": { "type": "string" }, "parent": target.clone(),
                "at": vec3("Where its origin goes (X and Z snapped for architecture)."), "turn": { "type": "number", "description": "Degrees about Y (quarter turns for architecture)." },
                "snap": { "type": "number", "minimum": 0, "description": "Grid step for at (default 0.5 m for architecture, 0 for props); 0 = free." },
                "material": material.clone(), "light": light.clone(), "visible": { "type": "boolean" },
                "length": { "type": "number" }, "height": { "type": "number" }, "width": { "type": "number" }, "depth": { "type": "number" }, "thickness": { "type": "number" },
                "block": { "type": "number" }, "course": { "type": "number" }, "gap": { "type": "number" }, "roughness": { "type": "number" }, "damage": { "type": "number" }, "unevenness": { "type": "number" }, "detail": { "type": "number" },
                "seed": { "type": "integer" }, "color": color.clone(),
                "opening": { "type": "number" }, "opening_height": { "type": "number" }, "arch": { "type": "string", "enum": ["round", "pointed", "flat"] }, "ring": { "type": "number" },
                "steps": { "type": "integer" }, "rise": { "type": "number" }, "run": { "type": "number" },
                "pattern": { "type": "string", "enum": ["flagstone", "bricks", "cobble"] }, "kind": { "type": "string" },
                "size": vec3("Rock size."), "strata": { "type": "number" }, "facets": { "type": "integer" }, "count": { "type": "integer" }, "lit": { "type": "boolean" },
            },
        })),
        tool("scatter", "Place many copies of objects (rocks, bones, debris, candles) over an area or onto surfaces, with random size and turn, a density, a minimum spacing and areas to keep clear. \
            Copies are instances (a source and a transform), so a thousand bones cost one bone mesh: the viewport draws them instanced, render_film renders them (a candle's light comes with every copy), glTF exports reference one mesh from many nodes, and get_scene's budget counts their triangles. \
            The sources are hidden by default (their copies still show); move them aside. Copies lie on the highest surface of the on objects under them (floors, terrain, rubble; their copies too), or flat at y. Instances are kept in the scatter object (world space at creation); target re-places them (or adds with mode add).", json!({
            "type": "object",
            "properties": {
                "sources": { "type": "array", "items": target.clone(), "description": "Objects to copy (1..64)." },
                "target": target.clone(), "name": { "type": "string" },
                "mode": { "type": "string", "enum": ["replace", "add"], "description": "With target: replace its copies (default) or add more." },
                "on": { "type": "array", "items": target.clone(), "description": "Surfaces to drop copies onto." },
                "center": { "type": "array", "items": { "type": "number" }, "description": "World [x, z] of the area (default [0, 0])." },
                "size": { "type": "array", "items": { "type": "number" }, "description": "Rectangle [width, depth] (default [4, 4])." },
                "radius": { "type": "number", "description": "A disc instead of a rectangle." },
                "y": { "type": "number", "description": "Height without surfaces (default 0)." },
                "density": { "type": "number", "description": "Copies per square meter (default 1)." },
                "count": { "type": "integer", "description": "At most this many (default: density x area)." },
                "min_distance": { "type": "number", "description": "No two copies closer than this in meters (default 0.25)." },
                "scale": { "type": "array", "items": { "type": "number" }, "description": "Size range [min, max] (default [0.8, 1.2])." },
                "yaw": { "type": "number", "description": "Random turn range in degrees (default 360)." },
                "tilt": { "type": "number", "description": "Random tilt up to this many degrees (default 0)." },
                "align": { "type": "number", "description": "Lean with the surface 0..1 (default 0)." },
                "sink": { "type": "number", "description": "Push into the surface, meters (default 0)." },
                "avoid": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "Circles [x, z, radius] to keep clear." },
                "avoid_objects": { "type": "array", "items": target.clone(), "description": "Keep clear of these objects' footprints." },
                "weights": { "type": "array", "items": { "type": "number" }, "description": "Relative chance of each source." },
                "max_slope": { "type": "number", "description": "Steepest ground in degrees (default 50)." },
                "seed": { "type": "integer" },
                "hide_sources": { "type": "boolean", "description": "Hide the source objects (default true)." },
            },
        })),
        tool("kitbash", "Hard-surface detail parts (Call of Duty / Titanfall style): procedural, game-ready (chamfered hard edges, UVs, a material per role) and placed ON a target surface. \
            The three detail levels, quickly: (1) secondary forms with detail_pass panel (plates, seams, hatches), (2) tertiary with detail_pass bolt and greeble, cables with detail_pass cable, (3) then a few hand placements here: pistons behind knees, hoses at hips, handles, latches, lights, antennas, stencils. \
            action place (default): part is a kind name or {kind, style, size [along X, out of the surface, across] meters, count, radius, sag, clips, ends, text, lit, glow, paint, color, materials {role: material}, detail, seed} (these keys also work at the top level). \
            InsertMesh: part {object, op: add|carve, scale} puts any object's shape (as to_clay makes it) into a clay target at at / points: its +Y along the surface normal, its pivot on the surface (a carve sinks half of it: a hole), its world size times scale, X along along, turned by rotate, lifted by offset; remove {group} takes the blobs out. A mesh becomes one asset (welded and decimated once, in a background job like to_clay) that every spot places; into an unevenly scaled clay it goes one spot per call. \
            Kinds: bolt (hex|socket|rivet|countersunk|carriage), nut, washer, hinge (count knuckles), latch (draw|toggle|cam), clamp (over a pipe), handle (bar|loop|recessed), vent (louver|slots|grille|fan|mesh), hatch (handle|latch|plain|vented: seam, hinges, corner bolts), piston and cable, hose, corrugated_tube, pipe, strap (between points), light (round|rect|cage|strip, glowing lens; lit false = dark), antenna (whip|blade|stub), sensor (dome|camera|lidar), exhaust (stack|port), armor_plate (bolted|plain|layered), track_link (count links), gear, spring, connector, ammo_box, jerry_can (real size), net, stencil (text, A-Z 0-9 - / . :), seam (rect|line panel line), greeble (box|canister|pipes|junction|tank), clip. action catalog lists them all with default sizes. \
            Unsized parts scale with the asset they go on (a 6.5 m mech: 4.5 cm bolts; a 1 m rifle: 7 mm). target: the object to put it on (several: targets). \
            Where: at (a world point near the surface: it snaps to the nearest surface, or along normal when given), points (several), path + spacing (along a polyline), array {count, step [x,y,z]} | {count, to} | {grid [cols, rows], spacing [along, across]}, along (a direction for the part's X, or \"edge\": the nearest hard edge; with count_along it spreads copies along that edge), rotate (degrees about the normal), offset (lift off the surface). \
            Parts never float or sink: each is projected onto the surface and sunk just enough that no footprint corner floats on curved surfaces (max_sink, share of its size, default 0.5; reported). A part whose footprint would overhang an edge slides inward to the nearest spot within its radius where it fits (slid reports how far; slide false turns it off); a failed call says why. Paint comes from the surface under it (its material and color); other roles: metal, dark, rubber, chrome, glow, glass, brass, seam, marking, strap (kb_* built-ins a scene material of the same name replaces). \
            Between points (piston, cable, hose, corrugated_tube, pipe, strap): from and to (world; ends snap onto the surface, connectors and eyes sit on it) with via points, or points. Pistons run body at from, rod at to. \
            mirror: true also places a mirrored copy across x = mirror_x (default 0), landing on whatever surface is there (the other leg). \
            Parts attach to the target's bone (the bone it rides, or on rigged clay the bone moving that spot most), so they animate and export under the joint; repeated parts are instances of one hidden source (kitbash_library), drawn and exported instanced. \
            Two-bone parts: when the two ends of a piston, cable, hose, corrugated_tube, pipe or strap land on different bones of one rig (chest -> hand.L, thigh -> shin), each end rides its own bone and the part is solved again in every pose (hoses bend and hang, pistons slide and aim) in the viewport, film, render_animation and glTF (hoses skinned to both joints, pistons as cylinder and rod nodes). bones [first end, last end] picks them; two_bone false keeps one bone. action span {group | target, bones?} makes existing point parts two-bone (bones default: the nearest bone to each end). \
            Parts follow the object they sit on: moving, aligning, re-posing (pose_bone) or mirroring it (mirror_object) takes them along. target may be a kitbash group or its copies: parts on parts (bolts on a plate). \
            Every call makes a group (group names one; reply group); action remove {group} removes everything a call added; action list shows groups with copies and triangles. Replies: placed, copies, sources, attached bones, triangles_drawn and unique_triangles, mirrored, missed.", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["place", "remove", "list", "catalog", "span"] },
                "bones": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2, "description": "Two-bone parts: the bones of the first and last end." },
                "two_bone": { "type": "boolean", "description": "Between-point parts whose ends land on different bones ride both (default true)." },
                "orient": { "type": "string", "enum": ["surface", "path"], "description": "With path: path turns each copy's X along the path." },
                "part": { "description": "Kind name or part settings {kind, style, size, ...}.", "oneOf": [{ "type": "string" }, { "type": "object" }] },
                "target": target.clone(), "targets": { "type": "array", "items": target.clone() },
                "at": vec3("World point near the surface."), "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "World points (several spots, or the path of a point part)." },
                "path": { "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": "Polyline: copies every spacing along it." },
                "spacing": { "type": "number" },
                "array": { "type": "object", "description": "{count, step [x,y,z]} | {count, to [x,y,z]} | {grid [cols, rows], spacing [along, across]}" },
                "normal": vec3("Snap along this direction instead of to the nearest surface."),
                "along": { "description": "Direction for the part's X ([x,y,z]) or \"edge\".", "oneOf": [{ "type": "array" }, { "type": "string" }] },
                "count_along": { "type": "integer", "description": "With along edge: copies spread along the edge." },
                "rotate": { "type": "number" }, "offset": { "type": "number" }, "max_sink": { "type": "number" }, "slide": { "type": "boolean" },
                "from": vec3("Point parts: start (snapped)."), "to": vec3("Point parts: end (snapped)."), "via": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } },
                "snap": { "type": "boolean", "description": "Point parts: snap the ends onto the surface (default true)." },
                "mirror": { "type": "boolean" }, "mirror_x": { "type": "number" },
                "group": { "type": "string", "description": "Group tag: name for a new group, or the group to remove." },
                "style": { "type": "string" }, "size": vec3("Size [along X, out, across] in meters."), "count": { "type": "integer" }, "radius": { "type": "number" },
                "sag": { "type": "number" }, "clips": { "type": "integer" }, "ends": { "type": "boolean" }, "text": { "type": "string" }, "lit": { "type": "boolean" }, "glow": color.clone(),
                "paint": { "type": "string" }, "color": color.clone(), "materials": { "type": "object" }, "detail": { "type": "number" }, "seed": { "type": "integer" },
            },
        })),
        tool("detail_pass", "One-call auto detail over target surfaces, seeded and scale-aware (sizes follow the asset: a 6.5 m mech vs a 1 m rifle; scale multiplies them). Run one pass per detail level, then add a few hand placements with kitbash. \
            pass panel: secondary forms on large flat faces: each face is cut into panels (largest free rectangles, long ones split) that become raised chamfered plates with seams between them (style plates, default), engraved seam lines cut into clay (engrave: band blobs named <group>/seamN, clay remeshes) or seam strips (lines); bolts at the corners and along long seams; hatches (count, default 2). coverage (share of each face, default 0.6: the rest is left for greebles), thickness, bolt (style), min_area. \
            pass bolt: bolts along hard edges and around the border of every flat face (rounded edges too), set in from the edge on the larger face (side both for both faces; concave true for inside corners too); spacing (meters), size [diameter], bolt style. \
            pass greeble: vents, boxes, junction boxes, canisters, lights, handles, connectors, latches on flat areas with a density (per square meter at 6.5 m scale) or count and a size range [min, max] meters; never overlapping each other, decals, stencils/paint or parts already there; parts: [\"vent\", {kind, style, weight}]. \
            pass cable: hoses and cable bundles across joints (default every joint between two targets' bones, on the limb's inner side; centered joints at the back) with P-clips on the surface and connectors / crimped fittings at the ends; routes: [{joint: bone, side inner|outer|front|back, kind hose|cable|bundle|corrugated_tube, radius} or {from, to}], count per joint (default 2). Cables ride the parent side's bone. \
            Limits: facing [x,y,z] + max_angle (degrees, default 75) keeps only faces turned that way; area {center, radius} a world sphere. mirror: true runs on the +X side and mirrors across x = mirror_x onto the other side's parts (exactly symmetric). budget: most triangles drawn (lowest priority parts dropped first: bolts, then greebles, then plates). materials {role: name} for every part; paint and color replace the surface's paint (gunmetal machinery on a painted hull). \
            Parts are instances in groups on the targets' bones (see kitbash); the reply gives the group (remove it with kitbash action remove), placed, copies, triangles_drawn, unique_triangles, flat_regions, hard_edges, and what was skipped and why.", json!({
            "type": "object",
            "properties": {
                "pass": { "type": "string", "enum": ["panel", "bolt", "greeble", "cable"] },
                "target": target.clone(), "targets": { "type": "array", "items": target.clone() },
                "seed": { "type": "integer" }, "scale": { "type": "number" }, "density": { "type": "number" }, "count": { "type": "integer" }, "spacing": { "type": "number" },
                "size": { "type": "array", "items": { "type": "number" } }, "style": { "type": "string" }, "bolt": { "type": "string" },
                "parts": { "type": "array" }, "facing": vec3("Only faces turned this way."), "max_angle": { "type": "number" },
                "area": { "type": "object", "description": "{center [x,y,z], radius}" }, "min_area": { "type": "number" }, "hatches": { "type": "integer" },
                "side": { "type": "string", "enum": ["larger", "both"] }, "coverage": { "type": "number" }, "concave": { "type": "boolean" }, "thickness": { "type": "number" },
                "routes": { "type": "array" }, "materials": { "type": "object" }, "paint": { "type": "string" }, "color": color.clone(),
                "mirror": { "type": "boolean" }, "mirror_x": { "type": "number" }, "budget": { "type": "integer" }, "group": { "type": "string" },
            },
            "required": ["pass"],
        })),
        tool("edit_terrain", "Shape and paint a Terrain (create_object shape Terrain makes one: a heightfield for cave floors and outdoor ground, size [w, d] centered on its origin). settings merge first: size, resolution (cells on the long side, 8..512, default 128), height (noise height m, 0.25), frequency (bumps per m, 0.35), octaves (1..8), ridged (0 rolling .. 1 sharp crests), cracks (crack depth m, 0 = none), crack_scale (crack network cell size, 0.9), crack_width (0.025), seed, color (flat ground sRGB), rock_color (slopes). \
            Then strokes in order: brush raise|lower (strength meters), smooth|flatten (strength 0..1; flatten toward height or the height under the first point), noise (strength meters, scale bump size), paint|erase (color sRGB, strength 0..1); points are [x, z] or [x, y, z] in the terrain's space (space world for world points), radius meters. Strokes are stored as offset and color grids saved with the scene.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "settings": terrain_settings.clone(),
                "strokes": { "type": "array", "items": { "type": "object", "properties": {
                    "brush": { "type": "string", "enum": ["raise", "lower", "smooth", "flatten", "noise", "paint", "erase"] },
                    "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } },
                    "radius": { "type": "number" }, "strength": { "type": "number" }, "height": { "type": "number" }, "color": color.clone(), "scale": { "type": "number" } }, "required": ["brush", "points"] } },
                "space": { "type": "string", "enum": ["local", "world"] },
                "clear_sculpt": { "type": "boolean" }, "clear_paint": { "type": "boolean" },
            },
            "required": ["target"],
        })),
        tool("frame_view", "Point a view's camera at one object (and its children), or at everything when target is omitted.", json!({ "type": "object", "properties": { "target": target.clone(), "view": { "type": "string", "enum": ["persp", "top", "front", "side"] } } })),
        tool("screenshot", "PNG image of the 3D viewport as it looks right now. Set whole_window to see the full editor UI.", json!({ "type": "object", "properties": { "whole_window": { "type": "boolean" } } })),
        tool("run_mel", "Run a line of MEL in the editor's command line (subset: polyCube/polySphere/polyCylinder/polyCone/polyPlane/polyTorus [-n name], select [-add|-tgl|-d|-cl|-all|-hi] names, move/rotate/scale [-r] x y z [names], delete, duplicate, hide, showHidden [-all], group [-em] [-n name] [names], ungroup, parent children... parent, parent -w, xform -cp | -piv x y z, setAttr name.attr value, ls [-sl], currentTime n, undo, redo, file -new; without names commands use the selection). Separate commands with ';'.", json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] })),
        tool("set_display", "Viewport display: shading mode (matcap: a sculpt matcap, built in or an image), grid, joints (rig skeletons), lighting (render_film's environments and lights), ambient occlusion, key-light shadows, HDR finishing (bloom, exposure, tonemap), single or four-view layout, and which camera the single view shows.", json!({ "type": "object", "properties": { "shading": { "type": "string", "enum": ["wireframe", "shaded", "wireframe_on_shaded", "matcap"], "description": "matcap shades every fill with a matcap (a lit sphere looked up by the normal as the camera sees it; creases on sculpted clay darker), as sculpting apps do." }, "matcap": { "type": "string", "description": "The matcap: clay (default), red_wax, skin, metal, or a square PNG/JPEG of a shaded sphere (at most 1024 a side). Turns matcap shading on unless shading says otherwise." }, "ao": { "description": "Short for ambient_occlusion (the same values).", "oneOf": [{ "type": "boolean" }, { "type": "object" }] }, "ao_radius": { "type": "number", "minimum": 0.01, "maximum": 10, "description": "Ambient occlusion radius in world units (ambient_occlusion.radius)." }, "grid": { "type": "boolean" }, "bones": { "type": "boolean" }, "baked": { "type": "boolean", "description": "Show baked game meshes (see bake) instead of clay." }, "brush_cursor": { "type": "boolean", "description": "Draw the brush tools' ring on the surface under the mouse (default true)." }, "overlay": { "type": "string", "enum": ["none", "mask", "groups"], "description": "How clay shows its sculpt mask and polygroups: mask (default: masked surface darker), groups (each group tinted, and the mask), none. Hidden groups are left out in every mode. Viewport only (render_film, bakes and exports are untouched); not an undo step." }, "fine": { "type": "boolean", "description": "Show clay with a detail_cell at that fine size (pores, wrinkles) instead of the mesh resolution. Slower to build the first time." }, "layout": { "type": "string", "enum": ["single", "four"] }, "view": { "type": "string", "enum": ["persp", "top", "front", "side"] }, "shadows": { "description": "Real-time key-light shadows in the viewport, render_views and render_animation (render_film always traces its own). Default on. true/false, or an object changing some settings. Scenes over the triangle budget draw without them; the reply's shadows.last_frame says whether the last frame had them and why not.", "oneOf": [{ "type": "boolean" }, { "type": "object", "properties": { "enabled": { "type": "boolean" }, "soft": { "type": "boolean", "description": "Contact-hardening penumbrae (PCSS) that widen away from the caster, instead of a fixed soft edge." }, "quality": { "type": "string", "enum": ["normal", "high"], "description": "Shadow map size: 2048 (normal) or 4096 (high, sharper on large scenes)." }, "strength": { "type": "number", "minimum": 0, "maximum": 1, "description": "How much of the key light a shadow takes away (1)." }, "softness": { "type": "number", "minimum": 0.005, "maximum": 0.3, "description": "The key's angular radius in radians for soft shadows (0.12, as render_film's key)." }, "ground": { "type": "boolean", "description": "Catch shadows on the grid plane y = 0 where there is no floor (default true; off when objects reach below it)." }, "key_direction": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "Direction toward the key light, default [0.45, 0.75, 0.55] (render_film's key). Also moves the key's highlight. Applies to the studio and classic lighting; other lighting shadows its brightest sun." } } }] }, "lighting": { "description": "What lights the viewport, render_views and render_animation: render_film's environments and lights, so the viewport previews a film look (diffuse light from the environment, reflections of it, its sun and render_film's studio lights as directional lights; the first casts the shadows). Default \"studio\" (render_film's default: the studio environment and its key, fill and rim lights, plus a viewport-only catchlight). An environment preset (overcast, sunset, golden_hour, forest, night, storm, or the physical sun and sky: physical, sky_noon, sky_afternoon, sky_golden, sky_sunset; as render_film environment, with its studio lights), a look preset (trailer, desert_heat, night_ops, ...), \"film\" (the last render_film's lighting), \"classic\" (the viewport's old fixed studio, no environment), or an object of render_film's lighting arguments {environment, lights, extra_lights, look} (e.g. {environment: \"sunset\", lights: \"none\"} or {environment: {preset: \"physical\", sun_elevation: 10}, lights: \"none\"}). Exposure, tonemap and grade are post's.", "oneOf": [{ "type": "string" }, { "type": "object", "properties": { "environment": {}, "lights": {}, "extra_lights": { "type": "array" }, "look": { "type": "string" } } }] }, "ambient_occlusion": { "description": "Screen-space ambient occlusion (contact darkening of the environment light in creases and under objects). Default on, radius 0.5, strength 1. true/false or {enabled, radius (world units 0.01..10), strength (0..2)}. Off for masks, classic lighting and scenes over the triangle budget; the reply's ambient_occlusion.last_frame says why.", "oneOf": [{ "type": "boolean" }, { "type": "object", "properties": { "enabled": { "type": "boolean" }, "radius": { "type": "number", "minimum": 0.01, "maximum": 10 }, "strength": { "type": "number", "minimum": 0, "maximum": 2 } } }] }, "post": { "description": "HDR finishing in the viewport, render_views and render_animation: a live preview of render_film's exposure, white balance, bloom (energy-conserving glare: only light above the threshold, such as emitters, torches and glowing windows, spreads), vignette, tonemap and grade. Default on: ACES and bloom {strength 0.08, threshold 1}, as render_film with bloom: true. Masks never change. true/false, or an object changing some settings (the rest stay; reset: true restores the defaults first). A look preset's environment, lights, fog, lens effects, grain and sharpening are render_film only. The reply's post echoes the settings.", "oneOf": [{ "type": "boolean" }, { "type": "object", "properties": { "enabled": { "type": "boolean", "description": "false draws as before HDR (no bloom or tonemap)." }, "look": { "type": "string", "enum": ["neutral", "trailer", "modern_warfare", "desert_heat", "night_ops", "cinematic_teal_orange"], "description": "render_film look preset (its tonemap, bloom, grade, vignette)." }, "tonemap": { "type": "string", "enum": ["aces", "agx", "filmic", "none"], "description": "none clips at white like the viewport before HDR." }, "exposure": { "type": "number", "minimum": -10, "maximum": 10, "description": "Stops brighter (+) or darker (-), default 0: render_film camera ev = 12 - exposure." }, "bloom": { "description": "As render_film: true (strength 0.08), false, a strength 0..1, or {strength, threshold}.", "oneOf": [{ "type": "boolean" }, { "type": "number", "minimum": 0, "maximum": 1 }, { "type": "object", "properties": { "strength": { "type": "number", "minimum": 0, "maximum": 1 }, "threshold": { "type": "number", "minimum": 0, "maximum": 100 } } }] }, "vignette": { "type": "number", "minimum": 0, "maximum": 1 }, "white_balance": { "description": "Kelvin 2000..12000 or [r, g, b] gains, as render_film.", "oneOf": [{ "type": "number" }, { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }] }, "grade": { "type": "object", "description": "As render_film's grade: look, saturation, contrast, lift, gamma, gain." }, "reset": { "type": "boolean" } } }] } } })),
        tool("edit_clay", "Change a Clay object's blobs. Steps run in this order: replace (new full list), remove, update (merge fields into existing blobs), add (append). Returns the blob names and the built mesh's bounds.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "replace": { "type": "array", "items": blob.clone() },
                "remove": { "type": "array", "items": blob_key.clone() },
                "update": { "type": "array", "items": { "type": "object", "description": "{blob: index or name, ...fields to change}", "properties": { "blob": blob_key.clone() }, "required": ["blob"] } },
                "add": { "type": "array", "items": blob.clone() },
                "before": { "type": ["integer", "string"], "description": "Insert the add blobs before this blob (index or name) instead of at the end. Later blobs win where they overlap, so put fillers before detail (lips, carves)." },
                "resolution": { "type": "integer", "minimum": 16, "maximum": 512, "description": "Grid cells across the longest side; see create_object." },
                "sharp": { "type": "boolean", "description": "Exact planar primitives, feature-preserving field placement and split crease shading (default true). false restores surface nets and shared field normals for comparisons. Hard forms target three cells across thicknesses/bevels at the default grid setting (192-512 cells across), honoring other explicit resolutions; detail_cell controls film/fine meshes." },
                "shell": { "type": ["number", "null"], "exclusiveMinimum": 0, "description": "Hollow the complete blob CSG inward by this thickness, before sculpt detail; null removes it. A shell form hollows just one blob." },
                "clear_detail": { "type": "boolean", "description": "Remove everything sculpted with brushes (keeps blobs)." },
                "detail_cell": { "type": "number", "minimum": 0.0001, "description": "Sculpt and paint cell size, finer than the mesh: e.g. 0.0005 (0.5 mm) on a head for pores, wrinkles, crisp lips and lids. Existing sculpt and paint are resampled. render_film, render_views fine: true and bake show it. Without detail regions the fine mesh is uniform and at most 2400 cells across (a too-fine value is refused with the smallest that fits); with detail_regions it is the cell everywhere outside them (e.g. 0.001 on a whole body, 0.004 on a 5-unit dragon) and the fine mesh is adaptive." },
                "detail_regions": { "type": "array", "maxItems": 16, "description": "Finer sculpt cells in parts of ONE clay, so a whole body can have 0.25 mm pores on the face and hands and 1 mm elsewhere (no need to split a head into its own object). Each: {name, center [x,y,z], radii [x,y,z] (or radius), cell, mirror (also add the copy across X = 0: name.L gives name.R)}; object space, like blobs; cell must be finer than detail_cell. The list replaces the current regions ([] removes them and their finer detail; the coarse grid keeps its own version of every stroke). Inside a region strokes land on its own grid; outside, detail fades to the coarser grid over a few coarse cells, so nothing steps. The fine mesh (render_film, render_views fine, bake) is then adaptive and crack-free: as fine as each region's cell only where something is sculpted or painted, the regular mesh elsewhere. Pick cells per region from the feature size: about 1/3 of the smallest feature (pores 0.8 mm -> 0.00025; knuckle creases 1.5 mm -> 0.0003-0.0005; dragon face scales 1.5 cm -> 0.002-0.003). Keep regions tight: triangles grow with region area / cell^2 (a face at 0.25 mm is about 1-2M triangles; the fine mesh is limited to 24M; a fine mesh that fails is listed in fine_mesh_errors and the regular mesh is drawn). create_human detail sets face and hand regions for you." },
            },
            "required": ["target"],
        })),
        tool("sculpt", "Works in the rest pose: points in and out are rest-pose points, even while a clip plays. Sculpt a Clay object with brush strokes, applied in order on a fine detail layer above the blobs. \
            Brushes: add / carve (melt on or cut a tube of radius along the points: ridges, brows, lips, grooves, creases), \
            inflate / deflate (push the surface out or in by up to radius * strength / 2: cheeks, muscles, hollows), \
            smooth (soften bumps and seams), flatten (press onto a plane through the stroke: planes of the face, soles), \
            grab (pull the surface by delta, at most radius: a nose forward, a chin down). Use stretch/extrude for thin tips: a continuous tapered deformation following delta, up to 8 radii; use a radius spanning the base and at least 2 mesh cells across the tip. crease makes a sharp V line (strength is depth in scene units, negative or invert makes a ridge); pinch tightens edges toward the path (strength 0-1); layer applies a fixed plateau height per stroke (strength is signed scene units), so overlapping dabs do not build up. \
            noise (texture over an area in one stroke: pores, bumps, wrinkles, overlapping scales or belly plates, see pattern and scale; set a detail_cell with edit_clay first, a few times finer than scale, and detail_regions where the features are finer than that). \
            standard raises the surface along its area normal dab by dab (invert carves): strength * radius / 2 at a lone dab's center; along a path the dabs are spaced and scaled so the ridge is about as high. smooth_mode peaks only lowers and valleys only fills; flatten side above only cuts, below only fills. \
            Every stroke takes a falloff curve (with focal_shift), per-point pen pressure with pressure curves, spacing, kind (dots, seeded spray, drag_rect, drag_dot), lazy, taper (crease + taper = Dam Standard), symmetry (mirror axes and radial copies; mirror: true = x) and a frame (world, object, surface, screen); paint: {color} also colors the stroke (MRGB). The reply lists dabs and symmetric copies per stroke. \
            With detail regions every stroke lands on the coarse grid and on each region it reaches, at that region's cell (radius_used is the finest), so fine features show inside regions and fade out at their edges. \
            Typical meter-scale settings: skin crease/layer depth 0.0005-0.002, pinch 0.2-0.5, pores strength 0.03-0.08 at scale 0.0008; cloth folds strength 0.5-0.9 at scale 0.02-0.05, crease depth 0.002-0.006; creature scales strength 0.3-0.7 at scale 0.03-0.06. Masks restrict every brush on the pre-stroke surface, including tiled strokes. A whole flank or tail takes one noise stroke: a path along it with a radius covering it. \
            Strokes run in the background: the call answers when they are applied (or with a job id after 40 s; see job_status); the mesh then rebuilds and later calls wait for it. \
            Points are world space by default; take them from compare_reference's pixel_to_world or the mesh bounds. \
            Detail stays in place in space: moving a blob later does not move what was sculpted on it. undo removes a whole call. \
            Clay-family brushes work toward the plane of the area under each dab (its mean surface): clay fills hollows up to it (square footprint, never cuts), clay_buildup fills them and lays a layer over it (strength sets the layer, about strength x radius / 10 per stroke), clay_tubes lays an even strip strength x radius / 2 high along the path (square, flat across; overlapping dabs do not build up), trim_dynamic cuts what stands above the plane with a sharp edge (never adds), polish (hPolish) presses both ways onto it. invert makes clay and clay_buildup cut instead. \
            snakehook pulls a horn out of the surface along points (its first point on the surface, the rest out in the air, at most 16 radii long; or one point and delta); curl_deg curls it about normal (default level, across the pull): the tip ends curl_deg degrees from the base direction. \
            alpha: {name (alpha import) or hash, depth, mid, rotate, scale, mode stamp|drag|tile, use height|mask} shapes dab brushes (standard, clay, clay_buildup, trim_dynamic, polish): with standard and use height the dab rises depth x (gray - mid); use mask scales any dab brush's weight. \
            vdm stamps a VDM (alpha create_vdm; alpha: {name, rotate, scale, mode stamp|drag}) on the surface under each dab, overhangs and all: the stamp's width is the dab's (radius = the captured radius stamps it at its own size), its up the area normal; strength (default 1) blends it in, invert carves it, spacing defaults to 2 (a stamp's width apart); kind drag_dot or a single point stamps once. \
            preset (call-level, or per stroke) names a brush preset (brush_preset list: ClayBuildup, ClayTubes, DamStandard, TrimDynamic, hPolish, Pores ... and the scene's own); the stroke's own fields override it and the record keeps the merged stroke. \
            The clay's persistent mask (mask tool) holds every brush back: each point moves by 1 - mask, and hidden polygroups (group tool) are skipped; mask: {channel: false} ignores it, {invert_channel: true} works on the masked part instead, {groups: [ids]} keeps to some groups, and auto-masking {backface, normal_angle, cavity, connected} reads the surface around each dab (connected is Move Topological). Each record keeps the mask and groups it read, so it replays the same after they change. \
            transform (brush) moves, turns or scales the unmasked part by transform {translate, rotate, scale, pivot} (the gizmo sends it when the clay has a mask): mask the part to keep still, transform the rest. \
            Every stroke is a record on its layer's stroke ledger (base unless layer says) (reply: record_ids, ledger_head, per-stroke changed_points, bricks, bbox, max_disp, mean_disp; volume_delta with measure): stroke_log lists them, stroke_edit disables, scales (Adjust Last: {action: scale, last: true, factor}), replaces or replays them, sculpt_diff measures changes. dry_run: true applies to a copy and reports without changing the clay.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "space": { "type": "string", "enum": ["world", "local"], "description": "Coordinate space of points, delta, normal (default world)." },
                "layer": { "type": "string", "description": "The sculpt layer to write: base or one made with sculpt_layer add (default: the clay's recording layer, sculpt_layer record, else base). A stroke reads every enabled layer (what you see) and writes its own: the change divided by the layer's amount goes into it, so the surface moves as the brush says; refused below amount 0.05." },
                "dry_run": { "type": "boolean", "description": "Measure only: run the strokes on a copy and reply with the records' stats; nothing changes." },
                "cell": { "type": "number", "description": "Sculptris Pro for every stroke without its own cell (see the strokes' cell)." },
                "level": { "type": "integer", "minimum": 0, "description": "The detail level (detail_level) for every stroke without its own level (default: the clay's active level, when it has levels)." },
                "measure": { "type": "boolean", "description": "Also measure volume_delta per stroke (slower)." },
                "label": { "type": "string", "description": "A name for these records and the undo step." },
                "pressure_source": { "type": "string", "enum": ["touch_force", "nsevent", "velocity"], "description": "Where a UI stroke's pen pressure came from (kept on its records; the editor sends it)." },
                "preset": { "type": "string", "description": "A brush preset under every stroke (brush_preset list); each stroke's own fields win." },
                "strokes": { "type": "array", "maxItems": 64, "items": {
                    "type": "object",
                    "properties": with_controls(json!({
                        "brush": { "type": "string", "enum": ["add", "carve", "inflate", "deflate", "smooth", "flatten", "grab", "noise", "crease", "pinch", "layer", "stretch", "extrude", "standard", "clay", "clay_buildup", "clay_tubes", "trim_dynamic", "polish", "snakehook", "vdm", "paint", "transform"] },
                        "transform": { "type": "object", "additionalProperties": false, "description": "transform brush: move, turn and scale the clay's material (in the call's space), each point by 1 - mask: what the gizmo sends with a mask (Transpose). Scale, then rotate (degrees about X, then Y, then Z), both about pivot, then translate; or a whole affine matrix (16 numbers, column-major). strength 0..1 (default 1) is how much of the move applies; radius is not needed. Offsets cap at 127 detail cells: big moves belong to blobs.", "properties": {
                            "translate": vec3("Move."), "rotate": vec3("Degrees about X, Y, Z."), "scale": { "oneOf": [{ "type": "number" }, { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }] }, "pivot": vec3("Turn and scale about this point."), "matrix": { "type": "array", "items": { "type": "number" }, "minItems": 16, "maxItems": 16 } } },
                        "preset": { "type": "string", "description": "A brush preset under this stroke (over the call's preset); the stroke's fields win. With a preset, brush may be left out." },
                        "alpha": alpha_schema.clone(),
                        "curl_deg": { "type": "number", "minimum": -720, "maximum": 720, "description": "snakehook: degrees the horn curls from base to tip." },
                        "cell": { "type": "number", "description": "Sculptris Pro: a detail cell finer than the clay's. Before the stroke, a detail region of that cell grows over it (a same-cell region it overlaps grows to hold both; at most 16 regions, the smallest of the nearest cell merging into its neighbour), so the stroke writes the finer grid there. The reply's detail_regions counts them." },
                        "level": { "type": "integer", "minimum": 0, "description": "The detail level the stroke works at (detail_level lists them; 0 is the clay's own cell). It writes that level and the coarser ones; finer detail under it on the layer it writes is carried along (a grab, stretch, snakehook or transform moves it with the material, other brushes move it with the surface), so pores made at a finer level survive coarse edits. A level above 0 sets cell to its own (Sculptris Pro). Default: the clay's active level when it has levels and the stroke gives no cell; records keep it." },
                        "pattern": { "type": "string", "enum": ["pores", "bumps", "wrinkles", "scales", "plates", "folds"], "description": "noise: pores (small pits: skin), bumps (soft lumps), wrinkles (fine lines across normal; default horizontal), scales (overlapping reptile/dragon scales: each one domed, rising to a free edge that overlaps the next one along normal), plates (belly plates: bands across normal, each overlapping the next). For scales and plates, normal is the way their free edges point (default down; e.g. toward the tail). folds are soft raised ridges running along normal (default gravity [0,-1,0]), winding a little; faces turned along normal (a sleeve end) stay smooth. Height is about strength x scale / 2." },
                        "scale": { "type": "number", "description": "noise: spacing of the features (default radius / 10): one scale or plate across. Skin pores about 0.001 on a 1.7-unit person; dragon body scales about 0.03-0.06 on a 5-unit dragon, face and limb scales smaller, belly plates larger. Set a detail_cell several times finer first." },
                        "point": vec3("A single dab."),
                        "radius": { "type": "number", "description": "Brush size (world units)." },
                        "strength": { "type": "number", "description": "Default 0.5 for traditional brushes (0-1). Crease: signed groove depth in scene units; layer: signed raised height in scene units; specify 0.001 for facial detail. Ignored by grab/stretch." },
                        "invert": { "type": "boolean", "description": "Reverse crease or layer sign (crease becomes a ridge, layer lowers); standard carves (default false)." },
                        "smooth_mode": { "type": "string", "enum": ["gentle", "normal", "peaks", "valleys"], "description": "smooth: gentle (half as strong), normal, peaks (only lowers bumps), valleys (only fills dips)." },
                        "side": { "type": "string", "enum": ["above", "below", "both"], "description": "flatten: above only cuts down to the plane, below only fills up to it (default both)." },
                        "paint": { "type": "object", "additionalProperties": false, "description": "Also color the stroke as it sculpts (MRGB): the same coverage as a paint stroke with this spec.", "properties": { "color": color.clone(), "hardness": { "type": "number", "minimum": 0, "maximum": 1 }, "strength": { "type": "number", "minimum": 0, "maximum": 1, "description": "Opacity (default 1)." }, "erase": { "type": "boolean" } } },
                        "mask": stroke_mask.clone(),
                        "delta": vec3("grab/stretch: the pull; stretch allows up to 8 radii and tapers its influence around the moving path."),
                        "normal": vec3("flatten: plane normal; crease/layer/pinch: surface normal (inferred at path center); noise folds: fold direction (default gravity); scales/plates: free-edge direction."),
                        "mirror": { "type": "boolean", "description": "Also apply it mirrored across the object's X = 0 (both brows, both cheeks)." },
                        "snap": { "type": "boolean", "description": "Move every point onto the nearest clay surface first (no guessing how deep the surface is)." },
                    })),
                    "description": "radius is required but for the transform brush.",
                } },
            },
            "required": ["target", "strokes"],
        })),
        tool("paint", "Works in the rest pose: points in and out are rest-pose points, even while a clip plays. Paint color onto a Clay object, over its blob colors. Steps run in order: clear (remove all paint), fill (one color over the whole surface), strokes. \
            A stroke is a soft round brush: a single point or a path, painting surfaces within radius. Use it for eyes, lips, spots, stripes, clothing edges. \
            Points are world space by default. Single-point dabs snap to the rest surface by default (snap:false opts out). Strokes take the same falloff, pressure, spacing, kind (dots, spray), lazy, taper, symmetry and frame controls as sculpt strokes. mask:{blobs:[names],above:y,below:y} restricts a stroke to named blobs or local height, as sculpt does. Painted channels can drive metallic and roughness in smart layers. Paint stays in place in space. \
            With channel, the steps paint a named mask channel instead of color (white = 1; color is not needed): smart material layers read it with a painted mask term {source: painted, channel} (where blood, moss or wear go by hand). At most 4 channels per clay.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "channel": { "type": "string", "description": "Paint this mask channel instead of color (see smart material layers)." },
                "label": { "type": "string", "description": "A name for the color strokes' records (the paint layer's stroke ledger; see stroke_log layer paint)." },
                "pressure_source": { "type": "string", "enum": ["touch_force", "nsevent", "velocity"], "description": "Where a UI stroke's pen pressure came from (kept on its records; the editor sends it)." },
                "layer": { "type": "string", "description": "The paint layer: paint (default) or a paint layer made with sculpt_layer add {kind: paint}; layers lie over paint in order, each at its amount (opacity)." },
                "space": { "type": "string", "enum": ["world", "local"] },
                "clear": { "type": "boolean" },
                "fill": color.clone(),
                "fill_strength": { "type": "number", "minimum": 0, "maximum": 1 },
                "strokes": { "type": "array", "maxItems": 64, "items": {
                    "type": "object",
                    "properties": with_controls(json!({
                        "point": vec3("A single dab."),
                        "radius": { "type": "number" },
                        "color": color.clone(),
                        "strength": { "type": "number", "minimum": 0, "maximum": 1, "description": "Opacity, default 1." },
                        "hardness": { "type": "number", "minimum": 0, "maximum": 1, "description": "0 soft edge, 1 hard edge (default 0.5)." },
                        "erase": { "type": "boolean", "description": "Remove paint instead (color not needed)." },
                        "mask": stroke_mask.clone(),
                        "alpha": alpha_schema.clone(),
                        "mirror": { "type": "boolean", "description": "Also paint mirrored across the object's X = 0." },
                        "snap": { "type": "boolean", "description": "Move every point onto the nearest clay surface first (no guessing how deep the surface is)." },
                    })),
                    "required": ["radius"],
                } },
            },
            "required": ["target"],
        })),
        tool("decal", "Works in the rest pose. Stamp decals onto a Clay object: a shape projected onto the surface at a point, with a size and turn, changing color, roughness, metal, glow and relief. \
            Shapes: rune (carved glyph; seed picks the glyph; with emission and a color it glows), scratch (a cluster of fine scratches showing bright metal), splat (a blood splat with droplets), emblem (a ring around a star, raised), image (a PNG or JPEG file: its color and alpha). \
            Seen in the viewport (per vertex, coarse), render_film and bakes. Steps: clear, remove, then decals. Points are world space by default.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "space": { "type": "string", "enum": ["world", "local"] },
                "clear": { "type": "boolean", "description": "Remove every decal first." },
                "remove": { "type": "array", "items": { "type": "integer" }, "description": "Indices of decals to remove (see get_clay decals)." },
                "decals": { "type": "array", "maxItems": 64, "items": {
                    "type": "object",
                    "properties": {
                        "shape": { "type": "string", "enum": ["rune", "scratch", "splat", "emblem", "image"] },
                        "image": { "type": "string", "description": "shape image: absolute path of a .png, .jpg or .jpeg file." },
                        "point": vec3("Where the decal's center goes (moved onto the surface when normal is omitted)."),
                        "normal": vec3("The way the decal faces (out of the surface); default: the surface normal at point."),
                        "size": { "type": "number", "description": "Width across, scene units (e.g. 0.08 for a rune on a shield)." },
                        "rotate": { "type": "number", "description": "Turn in degrees." },
                        "color": color.clone(),
                        "roughness": { "type": "number", "minimum": 0, "maximum": 1 },
                        "metallic": { "type": "number", "minimum": 0, "maximum": 1 },
                        "emission": { "type": "number", "minimum": 0, "description": "Glow, in film (glowing runes: 3 with color [1, 0.4, 0.1])." },
                        "depth": { "type": "number", "minimum": -4, "maximum": 4, "description": "Relief: > 0 raised, < 0 cut in (default: rune -1, scratch -0.7, splat 0.25, emblem 0.9, image 0)." },
                        "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                        "seed": { "type": "integer", "minimum": 0, "description": "Picks the variant (which glyph, which splat)." },
                    },
                    "required": ["point", "size"],
                } },
            },
            "required": ["target"],
        })),
        tool("project_reference", "Project the reference image onto a Clay object like a slide projector, lined up exactly as compare_reference lines it up. \
            Only surfaces that face the view and are not hidden get color; sides and back keep theirs. Best after the shape matches (overlap 0.9+). \
            Returns reference | painted model images.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "view": view.clone(),
                "match": { "type": "string", "enum": ["height", "width"] },
                "strength": { "type": "number", "minimum": 0, "maximum": 1, "description": "Opacity, default 1." },
                "min_facing": { "type": "number", "minimum": 0, "maximum": 0.95, "description": "How directly a surface must face the view to get paint (default 0.3; it fades in over the next 0.25)." },
            },
            "required": ["target"],
        })),
        tool("get_clay", "List a Clay object's blobs with their index, name, and every field.", json!({ "type": "object", "properties": { "target": target.clone() }, "required": ["target"] })),
        tool("set_reference", "Load the image to sculpt from (absolute .png/.jpg path). Finds the subject from transparency or a plain background and returns the subject mask to check, plus a palette: the subject's main colors with their share and pixel position. Also overlays it on a viewport pane. Without path, changes only the overlay; clear: true removes it.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "threshold": { "type": "number", "description": "Background color difference that counts as subject, 1..255 (default 40)." },
                "view": { "type": "string", "enum": ["persp", "top", "front", "side"], "description": "Pane that shows the overlay (default front)." },
                "opacity": { "type": "number", "minimum": 0, "maximum": 1 },
                "show": { "type": "boolean" },
                "clear": { "type": "boolean" },
            },
        })),
        tool("render_views", "Render the whole scene from several cameras in one contact-sheet image (the reference first, if loaded). Off screen: the editor window does not change.", json!({
            "type": "object",
            "properties": {
                "views": { "type": "array", "items": view.clone(), "maxItems": 9, "description": "Default: front, right, back, three_quarter." },
                "size": { "type": "integer", "minimum": 64, "maximum": 1024, "description": "Pixels per tile (default 384)." },
                "grid": { "type": "boolean" },
                "include_reference": { "type": "boolean", "description": "Default true." },
                "center": vec3("Close-up: point to frame (default: the whole scene's center)."),
                "radius": { "type": "number", "description": "Close-up: radius around center to fit in each tile." },
                "frame": { "type": "number", "description": "Pose rigs at this time-slider frame for this render only (default: the rest pose, where sculpt, paint and surface_point work)." },
                "show_bones": { "type": "boolean", "description": "Draw rig skeletons on top (seen through the clay)." },
                "baked": { "type": "boolean", "description": "Show baked game meshes (see bake) instead of clay, as a game would." },
                "fine": { "type": "boolean", "description": "Show clay with a detail_cell at that fine size (pores, wrinkles) instead of the mesh resolution. Slower to build the first time." },
                "weights": { "type": "string", "description": "A bone name: color its object by that bone's skin weights (blue none, green half, red full)." },
                "lighting": { "description": "Lighting for this render only, as set_display lighting (default: the viewport's)." },
                "ambient_occlusion": { "description": "Ambient occlusion for this render only, as set_display ambient_occlusion (default: the viewport's)." },
            },
        })),
        tool("compare_reference", "Line the model up with the reference image (same height on screen, centered) from one view, and measure how well the silhouettes overlap. Returns overlap (IoU, 1 = perfect), the regions that are missing or extra, a pixel_to_world mapping to turn image features into coordinates, and reference | model | overlap images.", json!({
            "type": "object",
            "properties": {
                "view": view.clone(),
                "match": { "type": "string", "enum": ["height", "width"], "description": "Which size to line up (default height)." },
            },
        })),
        tool("rig", "Add, change or remove the bones of a Clay object's skeleton. Steps run in order: clear, remove, update, add, clear_weights, weights. \
            Coordinates are in the clay's own space, like blobs. A bone runs from head to tail and turns around its head; put each bone \
            inside the part it should move, along its middle, with a child's head on its parent's tail (hip -> spine -> chest -> neck -> head; \
            chest -> upper_arm.L -> forearm.L -> hand.L; hip -> thigh.L -> shin.L -> foot.L). The skin follows automatically: each vertex follows \
            the nearest bones it can reach through the clay. Returns each bone with moves_vertices and inside_clay, plus warnings. \
            Weight strokes use world coordinates in the rest pose, persist across rebuilds/save/load, and apply after automatic weights (including blob bone overrides). Without edits it just reports the rig.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "clear": { "type": "boolean", "description": "Remove every bone, clip and weight stroke first." },
                "details": { "type": "boolean", "description": "Reply with every bone's parent, head and tail (default: just [name, vertices it moves] per bone, and warnings)." },
                "clear_weights": { "type": "boolean", "description": "Clear stored weight strokes before painting." },
                "weights": { "type": "array", "maxItems": 1024, "items": { "type": "object", "properties": {
                    "bone": { "type": "string" },
                    "points": { "type": "array", "minItems": 1, "maxItems": 256, "items": vec3("World-space rest-pose brush path point.") },
                    "radius": { "type": "number", "exclusiveMinimum": 0 },
                    "strength": { "type": "number", "minimum": 0, "maximum": 1, "description": "add/subtract amount; replace target weight (0 removes the bone, 1 gives it all); smooth blend amount. Full effect within half the radius, fading to nothing at the radius." },
                    "mode": { "type": "string", "enum": ["add", "subtract", "replace", "smooth"], "description": "Adjust the named bone, redistributing other weights proportionally. smooth averages neighboring vertices for that bone. Results normalize to four influences." }
                }, "required": ["bone", "points", "radius", "strength", "mode"] } },
                "split": { "type": "boolean", "description": "Default true: each moving part (body, head, each limb) is its own closed shell, so a limb that swings far does not leave a dent where it met the body; joints look sewn on. false: one melted shell (smooth joints, dents on big swings)." },
                "remove": { "type": "array", "items": { "type": "string" }, "description": "Bone names; their children move up to the removed bone's parent." },
                "update": { "type": "array", "items": { "type": "object", "properties": {
                    "bone": { "type": "string" }, "name": { "type": "string", "description": "Rename." }, "head": vec3("New head."), "tail": vec3("New tail."),
                    "parent": { "type": ["string", "null"] }, "deform": { "type": "boolean" } }, "required": ["bone"] } },
                "add": { "type": "array", "items": { "type": "object", "properties": {
                    "name": { "type": "string", "description": "Use .L / .R (or _L/_R, Left/Right) suffixes for sides, so mirror works." },
                    "head": vec3("Start (the joint it turns around)."),
                    "tail": vec3("End."),
                    "points": { "type": "array", "items": vec3("Chain point."), "description": "Instead of head/tail: a chain of bones through these points, named name1, name2... (spines, tails, ears)." },
                    "parent": { "type": "string", "description": "Existing bone (or one earlier in this list)." },
                    "deform": { "type": "boolean", "description": "False for a helper bone that moves children but no skin (a root at the feet). Default true." },
                    "mirror": { "type": "boolean", "description": "Also add the other side's bone, mirrored across X = 0 (arm.L also makes arm.R)." } },
                    "required": ["name"] } },
            },
            "required": ["target"],
        })),
        tool("animate", "Set keyframes in one clip of a rigged Clay object. A key poses one bone at one time-slider frame: rotate (degrees [x,y,z], around the bone's head, in the object's axes: x nods forward/back, y turns, z tilts sideways), \
            translate (offset from rest; usually only the root bone, for bounces and steps), scale (number or [x,y,z]). Channels left out keep the value they already have at that frame. \
            Between keys the pose eases smoothly (interp smooth), or moves evenly (linear), or holds (step). Keep each step between keys under 180 degrees. \
            A looping clip wraps from its last key to its first: give the first and last key the same pose. The clip becomes the one that plays (unless play: false) \
            and the time slider shows its range. Use render_animation to check the motion.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "clip": { "type": "string", "description": "Clip name (default \"action\"), e.g. idle, walk, wave. Created if new." },
                "keys": { "type": "array", "items": { "type": "object", "properties": {
                    "bone": { "type": "string" },
                    "frame": { "type": "number" },
                    "rotate": vec3("Degrees."),
                    "translate": vec3("Offset from rest."),
                    "scale": { "type": ["number", "array"] },
                    "interp": { "type": "string", "enum": ["smooth", "linear", "step"] },
                    "mirror": { "type": "boolean", "description": "Also key the other side's bone with the mirrored pose." } },
                    "required": ["bone", "frame"] } },
                "remove": { "type": "array", "items": { "type": "object", "properties": { "bone": { "type": "string" }, "frame": { "type": "number" } } }, "description": "bone + frame: one key; bone: its whole track; frame: every bone's key there." },
                "mirror": { "type": "boolean", "description": "Mirror every key in this call." },
                "loop": { "type": "boolean", "description": "Default true for new clips." },
                "play": { "type": "boolean", "description": "Make this the clip that plays (default true)." },
                "delete": { "type": "boolean", "description": "Delete the clip." },
            },
            "required": ["target"],
        })),
        tool("playback", "Control the time slider: go to a frame, play or stop, set fps, and pick which clip a rig plays (clip: null shows the rest pose, the pose it was sculpted in). Returns the clock and every rig's clips.", json!({
            "type": "object",
            "properties": {
                "frame": { "type": "number" },
                "play": { "type": "boolean" },
                "fps": { "type": "number", "minimum": 1, "maximum": 120 },
                "target": target.clone(),
                "clip": { "type": ["string", "null"], "description": "With target: the clip to play." },
            },
        })),
        tool("render_animation", "Render one view at several frames of the playing clips, as a contact sheet in frame order, with one fixed camera. Off screen, as a background job (like render_film): a long one answers with a job id; job_status {id, wait: 45} fetches the sheet. clip picks the clip (else the playing ones). save_path also writes the PNG. format png_sequence or mp4 also writes every frame (a movie through ffmpeg).", json!({
            "type": "object",
            "properties": {
                "view": view.clone(),
                "frames": { "type": ["integer", "array"], "description": "How many frames spread over the clip (2..16, default 8), or a list of frames. With format: up to 1200, default every whole frame of the clip; the sheet then shows 16 of them." },
                "size": { "type": "integer", "minimum": 64, "maximum": 1024, "description": "Pixels per tile (default 256, at most 512); with format the frame size (default 512, at most 1024, made even)." },
                "format": { "type": "string", "enum": ["png_sequence", "mp4"], "description": "Also write every frame: png_sequence = frame_00000.png .. in the folder path; mp4 = a movie at path (H.264 through an external ffmpeg from CLIDE_FFMPEG when set, else the PATH, /opt/homebrew/bin, /usr/local/bin or /usr/bin; without one the frames stay as a PNG sequence in <name>_frames next to it and the reply warns)." },
                "path": { "type": "string", "description": "With format: an absolute folder (png_sequence, made if missing) or an absolute .mp4 path." },
                "fps": { "type": "number", "minimum": 1, "maximum": 120, "description": "Movie frame rate (default the timeline's)." },
                "show_bones": { "type": "boolean", "description": "Draw the skeleton on top." },
                "clip": { "type": "string", "description": "Play this clip on every rig that has it for the render (the playing clip is restored after)." },
                "save_path": { "type": "string", "description": "Absolute .png path to also save the sheet to." },
                "center": vec3("Close-up center."),
                "radius": { "type": "number" },
            },
        })),
        tool("stroke_log", "List a Clay object's stroke records, oldest first: each sculpt (layer base) or paint (layer paint) stroke, each mask or group action and stroke (layers mask and groups), regions records for detail regions a stroke's cell grew (Sculptris Pro: kind regions, the regions not the grid), and opaque records for grid changes made outside strokes (an older file's detail, a new detail_cell, a paint fill). \
            A record has id, seq (its place), source (agent, ui, replay, import), tool, label, brush, radius, strength, dabs, bbox, enabled, factor, stale_context (the blobs or cell changed since it was made, so replaying it now gives another result), engine_stale (made by an older brush kernel) and stats {changed_points, bricks_touched, bbox, max_disp, mean_disp}. \
            full: true adds the stroke as sent (spec, resend it through sculpt after editing) and as resolved (the object-space dabs it applies); pages of 20. layer reports head, in_sync and replay_verified (whether replaying the records on load gave the saved grid).", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "layer": { "type": "string", "description": "base (default), paint, mask, groups, a sculpt or paint layer's name (sculpt_layer list), or archived:NAME (a baked or merged layer's records, read only)." },
                "from_seq": { "type": "integer", "minimum": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
                "ids": { "type": "array", "items": { "type": "integer" }, "description": "Only these records." },
                "full": { "type": "boolean" },
            },
            "required": ["target"],
        })),
        tool("stroke_edit", "Edit a Clay object's stroke records and replay the layer from the first one changed (from the nearest checkpoint, every 32 records): selective undo. Runs as a background job; one undo step. \
            Actions: disable / enable / delete {ids}; replace {id, spec: a new stroke, in the call's space; its level and cell fill as in sculpt, and a stroke that would grow a detail region is refused: replace cannot grow them} (on a kind {curve: {closed}} stroke, the same spec with a control point moved is MoveCurve: stroke_log full gives the spec); move {ids, to_seq}; scale {ids, factor 0..2} (how much of each stroke applies); scale {last: true, factor} is Adjust Last: the newest stroke this session made, rescaled from its own before and after at once (stale after undo or any other change: then scale by id); \
            rebase (replay every record with the current blobs and brush kernel and take the results as theirs: clears stale_context and engine_stale); replay {from_seq?} (a check: replay and report mismatched records; with none it installs the replayed grid, with some the clay keeps its own); squash {ids} (collapse records into one opaque record). Reply: replayed, from_seq, head, diff_stats of the layer, timing_ms.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "layer": { "type": "string", "description": "Default base; paint, mask, groups or a layer's name (sculpt_layer list). mask and groups hold the mask and group tools' actions and strokes (replace takes mask_paint strokes on mask; group strokes are painted again instead). Records on a layer read the others: changing another layer's amount or strokes marks them stale_context, and rebase replays them against the layers as they are." },
                "action": { "type": "string", "enum": ["disable", "enable", "delete", "replace", "move", "scale", "rebase", "replay", "squash"] },
                "ids": { "type": "array", "items": { "type": "integer" } },
                "id": { "type": "integer" },
                "spec": { "type": "object", "description": "replace: the new stroke (as in sculpt or paint strokes)." },
                "space": { "type": "string", "enum": ["world", "local"] },
                "to_seq": { "type": "integer", "minimum": 1 },
                "factor": { "type": "number", "minimum": 0, "maximum": 2 },
                "last": { "type": "boolean", "description": "scale: Adjust Last." },
                "from_seq": { "type": "integer", "minimum": 1 },
            },
            "required": ["target", "action"],
        })),
        tool("sculpt_diff", "Measure how a Clay object's base sculpt layer differs between two points of its ledger: from and to are a seq (the grid after that many records; 0 = none), {id: record} (after that record), \"blobs\" (no sculpting) or \"current\" (to's default); a seq or {id} is replayed as a background job. \
            Reply: changed_points, max_disp, mean_disp, rms, volume_delta, area_changed, bbox and a histogram of the changes; region {center, radius} limits it.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "from": { "description": "seq, {id}, \"blobs\", \"current\" or \"morph:NAME\" (that morph's grid of the layer)." },
                "layer": { "type": "string", "description": "The sculpt layer measured: base (default) or a sculpt layer's name." },
                "to": { "description": "Default current." },
                "region": { "type": "object", "properties": { "center": vec3("Center."), "radius": { "type": "number" } } },
                "histogram_bins": { "type": "integer", "minimum": 1, "maximum": 64 },
                "space": { "type": "string", "enum": ["world", "local"] },
            },
            "required": ["target", "from"],
        })),
        tool("probe_clay", "Read a Clay object's field: at points (up to 256), along rays {origin, direction} or from a view (screen: {view: front|back|left|right|top|bottom, at: [[h, v], ..]}). \
            Each probe: hit, point (the nearest surface point), distance (signed: + outside), inside, normal, curvature (mean, 1/radius, + on bumps), layer_offsets {base and each sculpt layer: millimetres it moves the surface there (a layer's at its amount)}, blob_owner (the blob that makes the surface there), mask (the sculpt mask there, 0..1), group (the polygroup id there, null for none) and group_visible. Works in the rest pose.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "points": { "type": "array", "maxItems": 256, "items": vec3("Point.") },
                "rays": { "type": "array", "items": { "type": "object", "properties": { "origin": vec3("Start."), "direction": vec3("Direction.") } } },
                "screen": { "type": "object", "properties": { "view": { "type": "string", "enum": ["front", "back", "left", "right", "top", "bottom"] }, "at": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 } } } },
                "space": { "type": "string", "enum": ["world", "local"] },
            },
            "required": ["target"],
        })),
        tool("history", "The undo history: one step per agent call (labelled like \"sculpt standard x3 on Head [agent]\") or continuous UI edit. \
            list: steps {index, label, source, tools, time, mark, bytes, undone}, current (the snapshot the scene is), dropped (old steps removed by the cap of 300 steps and 2 GB of grid data). \
            goto {index | mark}: undo or redo to the scene after that step (index = dropped is the scene before the first kept step). mark {index? (default current), name}: name a step (name null clears). label {index, name}. \
            quicksave {enabled?, now?}: QuickSave writes a backup every 5 minutes while there are unsaved changes, keeping the last 10 in the app's backups folder (an app setting, not saved in the scene); now writes one at once.", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "goto", "mark", "label", "quicksave"] },
                "index": { "type": "integer", "minimum": 0 },
                "mark": { "type": "string" },
                "name": { "type": ["string", "null"] },
                "enabled": { "type": "boolean" },
                "now": { "type": "boolean" },
            },
        })),
        tool("brush_preset", "Brush presets and the human's current brush. \
            Presets are a stroke's settings without its points (brush, strength, falloff, alpha, pattern, taper ...): 20 are built in (Standard, Clay, ClayBuildup, ClayTubes, DamStandard, TrimDynamic, hPolish, Inflate, Move, Pinch, Snakehook, Layer, Flatten, Planar, Smooth, SmoothPeaks, SmoothValleys, Pores, Wrinkles, Scales) and scenes save their own. \
            list: every preset {name, builtin, brush, radius, strength, falloff, alpha, kind}. get {name}: its settings. save {name, spec: settings} or {name, from_record: {target, layer, id}} (a stroke's settings from stroke_log) saves one in the scene (an undo step; built-in names are taken). delete {name} removes a saved one. \
            sculpt takes preset per call or per stroke; the stroke's own fields win and the record keeps the merged stroke. \
            get_current: the editor's brush tools' brush {tool (the editor's active tool), brush, radius, strength, invert, color, symmetry, preset, falloff, alpha, pressure, lazy, velocity, tablet, group} (UI state: not saved and not an undo step). set_current merges the given fields: tool (sculpt, paint, mask or groups switches the editor to that tool), \
            brush (a sculpt brush a drag can make: standard, clay, clay_buildup, clay_tubes, trim_dynamic, polish, add, carve, inflate, deflate, smooth, flatten, crease, pinch, layer, grab and snakehook (dragged across the view plane), vdm (stamps the alpha's VDM along the drag), mask_paint), preset (a preset's name, or null: the human's strokes go on top of it), radius (world units), strength, invert, color (the Paint tool's), symmetry ({axes: [x, y, z], radial} or null), falloff (as a stroke's, with focal_shift), alpha (a scene alpha's name, for dab brushes; a VDM stamp's for vdm), pressure (pressure curves {size, strength}), lazy (LazyMouse {radius, step (points), snap, backtrack} or null; the samples are smoothed, the request holds no lazy), velocity (pressure from pointer speed when no pen gives any), tablet (macOS tablet pen pressure monitor), group (the Groups tool's id). \
            A human drag sends a sculpt (paint, mask or group) request with these settings: Shift smooths, Alt inverts, Ctrl paints the mask (Ctrl+Alt erases), Ctrl+Shift draws a mask lasso; it shows in get_scene ui_activity, with pressure_source when a pen pressed.", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "get", "save", "delete", "get_current", "set_current"] },
                "name": { "type": "string", "description": "get, save, delete: the preset." },
                "spec": { "type": "object", "description": "save: the settings (a sculpt stroke without points, frame or preset)." },
                "from_record": { "type": "object", "description": "save: {target, layer (base or paint), id}: a record's stroke settings.", "properties": { "target": target.clone(), "layer": { "type": "string", "enum": ["base", "paint"] }, "id": { "type": "integer" } }, "required": ["target", "id"] },
                "preset": { "type": ["string", "null"], "description": "set_current: a preset the human's strokes go on (null: none)." },
                "tool": { "type": "string", "enum": ["sculpt", "paint", "mask", "groups"] },
                "brush": { "type": "string", "enum": ["standard", "clay", "clay_buildup", "clay_tubes", "trim_dynamic", "polish", "add", "carve", "inflate", "deflate", "smooth", "flatten", "crease", "pinch", "layer", "grab", "snakehook", "vdm", "mask_paint"] },
                "radius": { "type": "number", "exclusiveMinimum": 0 },
                "strength": { "type": "number" },
                "invert": { "type": "boolean" },
                "color": color.clone(),
                "symmetry": { "type": ["object", "null"], "description": "Stroke symmetry, as in sculpt strokes; null for none." },
                "falloff": { "description": "set_current: the falloff, as a sculpt stroke's (preset, curve, focal_shift); null: the brush's own." },
                "alpha": { "type": ["string", "null"], "description": "set_current: a scene alpha by name (dab brushes); null for none. It must be the brush's kind (a VDM stamp for vdm, an image for the others): a wrong one given is refused, and one kept from before a brush change of kind is dropped." },
                "pressure": { "type": ["object", "null"], "description": "set_current: pressure curves {size, strength}, as a sculpt stroke's." },
                "lazy": { "type": ["object", "null"], "description": "set_current: LazyMouse {radius (points, 0..200), step (points, 1..50, default 3), snap, backtrack}; null: off." },
                "velocity": { "type": "boolean", "description": "set_current: pointer speed gives pressure when no pen does." },
                "tablet": { "type": "boolean", "description": "set_current: the macOS tablet pen pressure monitor (window only)." },
                "group": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "set_current: the polygroup the Groups tool paints." },
            },
            "required": ["action"],
        })),
        tool("mask", "Works in the rest pose. A Clay object's persistent sculpt mask: 0 free to 1 frozen on its surface; every sculpt and paint stroke moves each point by 1 - mask (see sculpt mask), and the transform brush moves what is unmasked. The viewport shows masked surface darker (set_display overlay). \
            actions run in order, each a record on the mask ledger (stroke_log / stroke_edit layer mask): \
            {paint: [strokes]} (mask_paint strokes, the same controls as sculpt strokes: strength is how far the mask rises, invert lowers it; MaskPen preset); \
            {lasso: {view: front|back|left|right|top|bottom, at: [[h, v], ..] world units} or {camera: {view, proj, viewport}, px: [[x, y], ..]}, mode, through} and {rect: the same with from and to} (surface inside the outline as the camera sees it; through: false only takes surface facing the camera; the record stores the camera matrices, so resending it selects the same wherever the viewport went); \
            \"invert\", \"clear\", \"all\"; {grow: cells}, {shrink: cells}, {blur: cells} (along the surface band; blur keeps the mask's sum), {sharpen: amount}; \
            {by_cavity: {radius?}}, {by_curvature: {radius?}} (the smart materials' cavity and edge estimates), {by_normal: {dir, angle}}, {by_height: {axis x|y|z|[x,y,z], min, max}}, {by_blobs: {names}}, {by_groups: {ids}}, {connected: {seed_point}} (the surface joined to a point); \
            selections take mode set (default), add or sub. {from_channel: name} and {to_channel: name} copy from and to a smart-material mask channel (paint channel). \
            Reply: coverage (area-weighted share of the surface masked), bricks, bbox, records (one per action). Runs as a background job; one undo step.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "space": { "type": "string", "enum": ["world", "local"], "description": "Space of points, directions and heights (default world)." },
                "label": { "type": "string" },
                "pressure_source": { "type": "string", "enum": ["touch_force", "nsevent", "velocity"], "description": "Where a UI stroke's pen pressure came from (kept on its records; the editor sends it)." },
                "actions": { "type": "array", "minItems": 1, "maxItems": 64, "items": { "description": "An action name (\"invert\", \"clear\", \"all\") or an object with one action: {grow: 3}, {lasso: {..}}, {paint: [..]}.", "oneOf": [{ "type": "string", "enum": ["invert", "clear", "all", "sharpen"] }, { "type": "object", "minProperties": 1, "maxProperties": 1 }] } },
            },
            "required": ["target", "actions"],
        })),
        tool("group", "Works in the rest pose. A Clay object's polygroups: ids on its surface (spatial labels, so they survive remeshing; no group edges), each with a name and visibility. Brushes skip hidden groups and the viewport leaves them out; set_display overlay groups tints each. \
            actions run in order, each a record on the group ledger (stroke_log / stroke_edit layer groups): \"from_blobs\" (one group per blob), {from_mask: {threshold 0..1 (0.5), id?}} (the masked surface becomes a group), {from_normals: {angle}} (regions whose normals stay within angle of where each began), {paint: {id, strokes}} (group_paint strokes: surface the brush covers at half weight takes id), {rename: {id, name}}, {show: {ids}}, {hide: {ids}}, {isolate: {ids}}, \"show_all\", \"clear\", \"list\". \
            Reply: groups [{id, name, visible, area}], hidden. Runs as a background job (list alone answers at once).", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "space": { "type": "string", "enum": ["world", "local"] },
                "label": { "type": "string" },
                "pressure_source": { "type": "string", "enum": ["touch_force", "nsevent", "velocity"], "description": "Where a UI stroke's pen pressure came from (kept on its records; the editor sends it)." },
                "actions": { "type": "array", "minItems": 1, "maxItems": 64, "items": { "oneOf": [{ "type": "string", "enum": ["from_blobs", "show_all", "clear", "list"] }, { "type": "object", "minProperties": 1, "maxProperties": 1 }] } },
            },
            "required": ["target", "actions"],
        })),
        tool("sculpt_layer", "Works in the rest pose. A Clay object's layers, bottom first: base (its own sculpt) and paint (its own paint), then up to 14 more, each a sculpt or paint layer with an amount, a switch and a stroke ledger of its own (stroke_log / stroke_edit layer NAME). \
            The clay shows base + sum(amount x layer) over enabled sculpt layers, and paint layers laid over paint by coverage x amount; meshes, bakes, films and exports take the composite. A sculpt stroke on a layer reads every enabled layer and writes its own (stored divided by its amount, so amount 1 is what the brush did, 0 hides it, 2 doubles it). \
            Actions: list; add {name, kind: sculpt|paint (default sculpt), amount (default 1)}; amount {name, amount: -2..2, paint layers 0..1}; enable / disable {name}; rename {name, to}; move {name, to: place above base and paint, 1 = lowest}; \
            bake {name} (Bake Current: fold the layer into base, or paint, at its amount: the surface stays; its records are kept for reading as archived:NAME); merge_down {name} (fold into the layer below of its kind); delete {name}; \
            split_by_mask {name (default base), to (default \"NAME masked\")} (the masked part of a sculpt layer moves to a new layer above it: needs a mask); record {name} (the sculpt layer sculpt calls without layer write, saved with the clay; null: base); select {name} (the editor's Sculpt and Paint tools stroke it: editor state, not an undo step). \
            base and paint stay at amount 1. Layers, morphs, mask and groups share one grid budget in memory (grid_mb, max_grid_mb: 512 MB), and a clay with layers or morphs may save at most 1 GiB of grids (saved_mb, max_saved_mb), counting its ledgers' outside-edit grids and every morph again (a file does not share): about 3 morphs of a full-size sculpt (262,144 bricks save about 260 MB), fewer once its ledgers hold outside-edit grids (a detail_cell change records the whole grid; bake, blend and split_by_mask the bricks they changed). A call past either is refused and changes nothing. Reply: layers [{name, kind, amount, enabled, records, bricks, head, in_sync, stale_records, contributes}], record, selected, morphs, archived; amount answers composite_ms.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "action": { "type": "string", "enum": ["list", "add", "select", "amount", "enable", "disable", "rename", "move", "bake", "merge_down", "delete", "split_by_mask", "record"] },
                "name": { "type": ["string", "null"] },
                "kind": { "type": "string", "enum": ["sculpt", "paint"] },
                "amount": { "type": "number", "minimum": -2, "maximum": 2 },
                "to": { "description": "rename and split_by_mask: the new name; move: the new place (1 = lowest above base and paint)." },
            },
            "required": ["target", "action"],
        })),
        tool("detail_level", "Works in the rest pose. A Clay object's detail levels (like subdivision levels): level 0 is its own detail cell, finer levels are finer cells (up to 8), coarse first. \
            add: a cell finer than level 0 becomes a level and the active one. level (an index) or cell picks the active level (a cell not listed is added). With add and level both, level indexes the list after the add (coarse first, so the new cell may move the levels finer than it up one). With none of them it lists, changing nothing. \
            sculpt strokes without level work at the active level: a stroke at level k writes level k and the coarser ones (above 0 it grows a detail region of that cell over itself first, Sculptris Pro) and carries the written layer's finer detail along (other sculpt layers keep theirs), so a coarse grab or inflate keeps the pores made at a finer level. \
            The levels and the active one are saved with the clay. Adding a level is an edit (an undo step); picking the active one is selection-like and never an undo step (undo and redo keep it). Reply: levels [{level, cell, regions, active}], level, active_cell, step (the undo step an add made; null for a list or pick). D and Shift+D in the editor (with a brush tool, not during a stroke) go up and down a level.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "level": { "type": "integer", "minimum": 0, "description": "Make this level active (0: the clay's own cell). With add, an index into the list after the add." },
                "cell": { "type": "number", "description": "Make the level of this cell active, adding it when the clay has none." },
                "add": { "type": "number", "description": "Add a level of this cell (finer than level 0) and make it active." },
            },
            "required": ["target"],
        })),
        tool("morph", "Works in the rest pose. A Clay object's morph targets: snapshots of its sculpt layers (base and every sculpt layer: grids, stroke ledgers, amounts, switches), shared rather than copied. \
            store {name} (the sculpt as it is now; again replaces it); switch {name} (swap the clay's sculpt layers with the morph's: the clay shows it and the morph holds what was there, so switching again goes back); \
            blend {name, weight 0..1} or {weights: {name: weight}} (move the sculpt toward morphs: each grid and amount to grid + weight x (morph - grid); recorded on the layers' ledgers as opaque records); delete {name}; list; diff {name}. \
            Morphs need the same sculpt layers as when stored. In memory a morph shares its grids, but a file writes each morph's grids and ledgers again, and a clay with layers or morphs saves at most 1 GiB of grids (sculpt_layer list: saved_mb, max_saved_mb): about 3 morphs of a full-size sculpt (262,144 bricks save about 260 MB), fewer once its ledgers hold outside-edit grids (a detail_cell change records the whole grid; bake, blend and split_by_mask the bricks they changed). A store, switch or blend past that is refused and changes nothing; delete morphs, bake layers or squash records to make room. Reply: morphs [{name, layers, changed_points, max, mean, rms (how far the morph's surface is from the clay's, as field offset), blobs_changed}]. export_glb and export_fbx take morph:NAME blend shapes.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(),
                "action": { "type": "string", "enum": ["store", "switch", "blend", "delete", "list", "diff"] },
                "name": { "type": "string" },
                "weight": { "type": "number", "minimum": 0, "maximum": 1 },
                "weights": { "type": "object", "additionalProperties": { "type": "number", "minimum": 0, "maximum": 1 } },
            },
            "required": ["target", "action"],
        })),
        tool("alpha", "Works in the rest pose. The scene's alpha images (16-bit grayscale, at most 1024 pixels a side) for sculpt strokes' alpha. \
            import {name, path}: a PNG (8 or 16 bit; color becomes luminance), JPEG or OpenEXR (channel Y, else R; values 0..1), at most 32 MiB, by absolute path; the same file gives the same hash. Importing a name again with other pixels rebinds it on every clay that used it: their records made with the old image report stale_context (stroke_edit rebase takes the new one). \
            from_view {name, target, view: front|back|left|right|top|bottom, size (16-1024, default 256), center [h, v], extent}: a height map of a clay seen from that side (nearest surface white, misses black), framed on the clay unless center and extent (half the square, world units) say. \
            create_vdm {name, target, center [x, y, z], radius, above?, below?, normal?, tangent?, cell?, space?}: a VDM stamp (Alpha 3D) captured from a clay as sculpted: the box radius across each way, from below (default radius / 2) under the surface to above (default 2 radius) over it (each at most 8 radii), standing on the surface at center (moved onto it along normal; normal defaults to the surface's there, tangent, the stamp's across, to X). It keeps what stands out of (or sinks into) the flat base plane, overhangs and all, as a sparse volume at cell (default the clay's detail cell, coarsened to at most 2M lattice points); brush vdm stamps it (sculpt). Reply adds vdm {base, normal, radius, above, below, cell, bricks} (the clay's own units). Its image (preview, list kind vdm) is its height seen from above. \
            list: {name, kind (image or vdm), hash, w, h, used_by}. delete {name}: from the library (clays keep the images their strokes use, so those still replay). preview {name, size}: the image as a PNG.", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["import", "from_view", "list", "delete", "preview", "create_vdm"] },
                "name": { "type": "string" },
                "path": { "type": "string", "description": "import: absolute path to a .png (8 or 16 bit), .jpg or .exr." },
                "hash": { "type": "string", "description": "preview: by content hash instead of name." },
                "target": target.clone(),
                "view": { "type": "string", "enum": ["front", "back", "left", "right", "top", "bottom"] },
                "size": { "type": "integer", "minimum": 16, "maximum": 1024 },
                "center": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 3, "description": "from_view: [h, v] in the view; create_vdm: [x, y, z], the stamp's base point on the clay." },
                "extent": { "type": "number", "exclusiveMinimum": 0 },
                "radius": { "type": "number", "exclusiveMinimum": 0, "description": "create_vdm: half the stamp's width." },
                "above": { "type": "number", "minimum": 0, "description": "create_vdm: how far over the surface it reaches (default 2 radius; at most 8 radii)." },
                "below": { "type": "number", "minimum": 0, "description": "create_vdm: how far under the surface it reaches (default radius / 2; at most 8 radii)." },
                "normal": vec3("create_vdm: the stamp's up (default the surface normal at center)."),
                "tangent": vec3("create_vdm: the stamp's across (default X, laid on the surface)."),
                "cell": { "type": "number", "exclusiveMinimum": 0, "description": "create_vdm: the lattice cell (default the clay's detail cell)." },
                "space": { "type": "string", "enum": ["world", "local"], "description": "create_vdm: what center, radius, normal are given in (default world)." },
            },
            "required": ["action"],
        })),
        tool("import_mesh", "Import a triangle mesh file: .obj, .stl (binary or ASCII), .ply (ASCII or binary), .glb (every mesh at its node's placement, rest pose) or binary .fbx (Geometry nodes in meters by UnitScaleFactor; model transforms are not applied). At most 1 GiB and 4M triangles; malformed files are refused. \
            as clay (default): a new clay with one mesh blob, the mesh decimated to at most 200k triangles for its distance field; a mesh that is not closed is read by its generalized winding number, so its holes close (close_holes false refuses it instead). Sculpt, paint, mask and layers work on it as on any clay; resolution sets its mesh. \
            as mesh: a Mesh object drawing the file's triangles (smooth normals, the object's material); close_holes (default true) fills boundary loops with fans. to_clay turns it into clay later, kitbash part {object} inserts it. \
            scale multiplies the file's units; up z turns a Z-up file to Y up; center moves its box center to the origin. Reply: object, triangles (the file's), triangles_kept, watertight, holes, holes_closed, bounds, warnings. File > Import… sends this call.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to .obj, .stl, .ply, .glb or .fbx." },
                "name": { "type": "string" },
                "as": { "type": "string", "enum": ["clay", "mesh"] },
                "scale": { "type": "number", "exclusiveMinimum": 0 },
                "up": { "type": "string", "enum": ["y", "z"] },
                "center": { "type": "boolean" },
                "close_holes": { "type": "boolean" },
                "resolution": { "type": "integer", "minimum": 16, "maximum": 512, "description": "as clay: its mesh resolution." },
            },
            "required": ["path"],
        })),
        tool("to_clay", "Works in the rest pose. Turn objects into clay, or combine them (Make PolyMesh3D, DynaMesh any mesh, Remesh All, booleans across objects). Primitives become analytic blobs (cube: box, sphere, cylinder and cone: lathe, torus; a plane a thin slab; sheared or unevenly scaled ones a mesh blob); clay, imported meshes and every other mesh become mesh blobs of their (composed rest) mesh, decimated to 200k triangles, open ones read by winding number. \
            Without into the first target is converted in place (it keeps its id, name, placement, children; a clay keeps its blobs and sculpt) and the others are added to it with op: add, carve (cut away) or intersect (keep only the overlap); with into (a clay), every target goes into it with op. Targets other than the result are hidden. \
            mode exact (default) keeps the blobs; mode volume remeshes the whole result at resolution into one closed mesh blob (DynaMesh: sculpt and layers folded in, paint and the old ledgers dropped), with blur (Gaussian, in cells, 0-4) and polish (curvature flow, 0-1) as sculpt offsets, volume kept. Ledgers settle first. \
            Reply: object, mode, op, blobs, cell, hausdorff_est and hausdorff_cells (one target: the largest distance from its surface to the clay's), hidden, notes. Modify > Remesh as Clay… sends mode volume.", json!({
            "type": "object",
            "properties": {
                "target": target.clone(), "targets": { "type": "array", "items": target.clone(), "maxItems": 64 },
                "op": { "type": "string", "enum": ["add", "carve", "intersect"] },
                "into": target.clone(),
                "mode": { "type": "string", "enum": ["exact", "volume"] },
                "resolution": { "type": "integer", "minimum": 16, "maximum": 512 },
                "blur": { "type": "number", "minimum": 0, "maximum": 4 },
                "polish": { "type": "number", "minimum": 0, "maximum": 1 },
            },
        })),
        tool("export_print", "Works in the rest pose. Export solids for 3D printing: .stl (binary), .obj, .ply (binary) or .3mf, one welded mesh of the targets (default every visible solid; groups take what is under them) in millimeters (1 scene unit = 1 m unless size_mm), Y up turned to Z up and set on the plate (lowest point z = 0). \
            size_mm {axis: x|y|z (scene axes), value}: scale so the model is that many millimeters along the axis. hollow {wall_mm} (clay only): hollowed inward to that wall. fine: clay with a detail_cell at its fine mesh. \
            Reply: triangles, vertices, watertight (every edge used by exactly two triangles, in opposite directions), non_manifold_edges, inconsistent_edges, degenerate_dropped, volume_cm3, bbox_mm {min, max, size}, mm_per_unit. File > Export for Print… sends this call.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path ending in .stl, .obj, .ply or .3mf." },
                "targets": { "type": "array", "items": target.clone() },
                "size_mm": { "type": "object", "properties": { "axis": { "type": "string", "enum": ["x", "y", "z"] }, "value": { "type": "number", "exclusiveMinimum": 0 } }, "required": ["value"] },
                "hollow": { "type": "object", "properties": { "wall_mm": { "type": "number", "exclusiveMinimum": 0 } }, "required": ["wall_mm"] },
                "fine": { "type": "boolean" },
            },
            "required": ["path"],
        })),
        tool("undo", "Undo the last edit (agent or human).", empty.clone()),
        tool("redo", "Redo the last undone edit.", empty.clone()),
        tool("new_scene", "Clear the scene. Undoable.", empty.clone()),
        tool("open_scene", "Load a scene from an absolute .json path. Undoable.", json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })),
        tool("export_glb", "Export to a glTF binary file (.glb) for Blender, Unity, Unreal, Godot or the web: every visible object (or targets and what is under them) with its colors \
            (blob colors, paint and object color as vertex colors), and for rigged clay the skeleton, skin weights and every clip as an animation (sampled once per frame). \
            Will not overwrite a file that is not a .glb.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path ending in .glb." },
                "targets": { "type": "array", "items": target.clone(), "description": "Objects to export (default: the whole scene)." },
                "animations": { "type": "boolean", "description": "Include clips (default true)." },
                "lods": { "type": "boolean", "description": "Baked objects also get LOD1-LOD3 (about 1/2, 1/4, 1/8 of the triangles, same UVs, textures and skin) as MSFT_lod levels on the LOD0 node: engines that read MSFT_lod swap them by screen size; other readers show LOD0 only. The textures keep the finest detail at every level. Default false." },
                "blend_shapes": { "type": "array", "maxItems": 32, "items": { "type": "string" }, "description": "Blend shapes (glTF morph targets, FBX BlendShape channels) on every exported clay that has them: layer:NAME (the exported mesh with that sculpt layer fully on; export with the layer off or at 0) or morph:NAME. Each rest vertex is moved onto the target surface (a few Newton steps), so vertex counts stay; a shape the vertices cannot follow (big pulls stretch the surface past them) is refused and its residual reported. Reply: blend_shapes [{object, name, residual, max_residual, vertices, refused?}]." },
                "max_residual": { "type": "number", "exclusiveMinimum": 0, "description": "Refuse a blend shape farther than this from its target surface, in scene units (default half the clay's mesh cell)." },
            },
            "required": ["path"],
        })),
        tool("export_fbx", "Export to a binary FBX 7.4 file (.fbx) for Maya, Unity, Unreal or Blender. Same content as export_glb (FBX has no standard slot for the AO/roughness/metal map, so baked meshes carry color and normal maps and the material's roughness and metal values): visible objects (or targets), \
            meshes with vertex colors and a material in each mesh's average color, and for rigged clay the skeleton, skin and every clip (one take each, sampled per frame). \
            One scene unit is one meter. Will not overwrite a file that is not a binary .fbx.", json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path ending in .fbx." },
                "targets": { "type": "array", "items": target.clone(), "description": "Objects to export (default: the whole scene)." },
                "animations": { "type": "boolean", "description": "Include clips (default true)." },
                "lods": { "type": "boolean", "description": "Baked objects become a LodGroup (NAME or NAME_LODGroup) of NAME_LOD0..NAME_LOD3 (about 1, 1/2, 1/4, 1/8 of the triangles; each skinned to the same skeleton): Unreal and Maya read the LodGroup, Unity groups the _LODn names. Default false." },
                "blend_shapes": { "type": "array", "maxItems": 32, "items": { "type": "string" }, "description": "Blend shapes (glTF morph targets, FBX BlendShape channels) on every exported clay that has them: layer:NAME (the exported mesh with that sculpt layer fully on; export with the layer off or at 0) or morph:NAME. Each rest vertex is moved onto the target surface (a few Newton steps), so vertex counts stay; a shape the vertices cannot follow (big pulls stretch the surface past them) is refused and its residual reported. Reply: blend_shapes [{object, name, residual, max_residual, vertices, refused?}]." },
                "max_residual": { "type": "number", "exclusiveMinimum": 0, "description": "Refuse a blend shape farther than this from its target surface, in scene units (default half the clay's mesh cell)." },
            },
            "required": ["path"],
        })),
        tool("save_scene", "Save the scene to an absolute .json path. Will not overwrite a file that is not a scene. Without path, saves to the current file.", json!({ "type": "object", "properties": { "path": { "type": "string" } } })),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_origins_only() {
        assert!(is_local_origin("http://localhost:3000"));
        assert!(is_local_origin("http://127.0.0.1"));
        assert!(!is_local_origin("https://evil.example"));
        assert!(!is_local_origin("http://localhost.evil.example"));
    }

    #[test]
    fn initialize_and_list_without_ui() {
        let (tx, _rx) = mpsc::channel();
        let ctx = egui::Context::default();
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
        let r = dispatch(&init, &tx, &ctx).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let notif = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(dispatch(&notif, &tx, &ctx).is_none());
        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let tools = dispatch(&list, &tx, &ctx).unwrap()["result"]["tools"].as_array().unwrap().len();
        assert_eq!(tools, 72);
    }
}
