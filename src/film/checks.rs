//! Reproducible lighting comparisons without opening the editor.
use super::*;
use serde_json::json;

/// SCENE=dragon.json cargo test --release light_check -- --ignored --nocapture
#[test]
#[ignore]
fn light_check() {
    let path = std::env::var("SCENE").expect("SCENE");
    let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let output = std::path::PathBuf::from(std::env::var("LIGHT_OUTPUT").unwrap_or_else(|_| "light_check".into()));
    std::fs::create_dir_all(&output).unwrap();
    let mut lib = crate::mesh::MeshLib::new();
    lib.show_fine = true;
    lib.sync(&scene);
    let eval = scene.evaluate();
    let items: Vec<Item> = scene.objects.iter().filter(|o| o.shape.is_mesh()).filter_map(|o| {
        let &(world, true) = eval.get(&o.id)? else { return None };
        Some(Item { mesh: lib.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
    }).collect();
    let bounds = items.iter().flat_map(|i| i.mesh.vertices.iter().map(|v| i.world.transform_point3(Vec3::from(v.pos)))).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let meshes = Meshes::new(items);
    let center = (bounds.0 + bounds.1) * 0.5;
    let radius = (bounds.1 - bounds.0).length() * 0.5;
    let direction = Vec3::new(0.65, 0.3, 0.7).normalize();
    let full = Camera { eye: center + direction * radius * 2.8, target: center, up: Vec3::Y, fov_deg: 30.0, ortho: None };
    let head = Vec3::new(0.08, 2.53, 2.08);
    let portrait = Camera { eye: head + Vec3::new(0.8, 0.12, 0.6).normalize() * 1.9, target: head, up: Vec3::Y, fov_deg: 30.0, ortho: None };
    // A tiny RGBE softbox map also exercises the actual file loader.
    let mut hdr = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 8 +X 16\n".to_vec();
    for y in 0..8 {
        hdr.extend([2, 2, 0, 16]);
        let row: Vec<[u8; 4]> = (0..16).map(|x| if (2..4).contains(&y) && (1..4).contains(&x) { [192, 160, 128, 132] } else { [128, 144, 160, 126] }).collect();
        for c in 0..4 { hdr.push(16); hdr.extend(row.iter().map(|p| p[c])); }
    }
    let hdr_path = output.join("softbox.hdr");
    std::fs::write(&hdr_path, hdr).unwrap();
    let hdr_path = hdr_path.canonicalize().unwrap();
    let cases = [
        ("studio", full, json!({})),
        ("overcast", full, json!({"environment":"overcast","lights":"none","background":"environment","tonemap":"agx"})),
        ("golden_hour", full, json!({"environment":"golden_hour","lights":"none","background":"environment","tonemap":"agx"})),
        ("forest", full, json!({"environment":"forest","lights":"none","background":"environment","tonemap":"agx"})),
        ("hdr_softbox", full, json!({"environment":{"hdr":hdr_path,"rotate":35,"intensity":1},"lights":"none","background":"studio","tonemap":"agx"})),
        ("head_pinhole", portrait, json!({"environment":"golden_hour","lights":"none","background":"environment","tonemap":"filmic","floor":false,"bloom":true,"vignette":0.2,"camera":{"focal_length_mm":85}})),
        ("head_dof", portrait, json!({"environment":"golden_hour","lights":"none","background":"environment","tonemap":"filmic","floor":false,"bloom":true,"vignette":0.2,"camera":{"focal_length_mm":85,"dof":{"f_stop":2.8,"focus_point":[0.17,2.56,1.985]}}})),
    ];
    let samples = std::env::var("LIGHT_SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(128);
    let settings = Settings { width: 640, height: 640, samples, bounces: 4, time_limit: Duration::from_secs(600), floor: true, exposure: 1.0, denoise: true };
    for (name, cam, args) in cases {
        if std::env::var("LIGHT_CASE").is_ok_and(|v| !v.split(',').any(|case| case == name)) { continue; }
        let look = Arc::new(Look::parse(&args).unwrap());
        let cam = look.camera.apply(cam).unwrap();
        let settings = Settings { floor: args["floor"].as_bool().unwrap_or(true), ..settings };
        let img = render_views_with_look(&meshes, &[], &[cam], settings, look, &Default::default()).pop().unwrap();
        image::save_buffer(output.join(format!("{name}.png")), &img.pixels.iter().flatten().copied().collect::<Vec<_>>(), 640, 640, image::ColorType::Rgb8).unwrap();
        let metadata = json!({"render_film":args,"size":640,"samples":img.samples,"bounces":settings.bounces,"exposure":settings.exposure,"camera_position":cam.eye.to_array(),"camera_target":cam.target.to_array(),"fov":cam.fov_deg,"tracing_seconds":img.seconds});
        std::fs::write(output.join(format!("{name}.json")), serde_json::to_vec_pretty(&metadata).unwrap()).unwrap();
        assert_eq!(img.samples, samples);
        eprintln!("{name}: {} samples in {:.1}s", img.samples, img.seconds);
    }
}

/// Reproduce the film-camera acceptance pictures from a copied scene.
/// SCENE=/absolute/mech.json cargo test --release film_look_check -- --ignored --nocapture
#[test]
#[ignore]
fn film_look_check() {
    let source=std::path::PathBuf::from(std::env::var("SCENE").expect("SCENE"));
    let output=std::path::PathBuf::from(std::env::var("LOOK_OUTPUT").unwrap_or_else(|_|"look_check".into()));
    std::fs::create_dir_all(&output).unwrap();
    let copy=output.join("mech.json");
    if source.canonicalize().unwrap()!=copy.canonicalize().unwrap_or_default() { std::fs::copy(&source,&copy).unwrap(); }
    let source_bytes=std::fs::read(&copy).unwrap();
    let hash=source_bytes.iter().fold(14695981039346656037u64,|h,b|(h^*b as u64).wrapping_mul(1099511628211));
    let key=format!("maya-film-native-v2:{hash:016x}:{}",std::mem::size_of::<crate::mesh::Vertex>());
    let cache=output.join("geometry.cache");
    let items=if let Some(items)=read_look_geometry(&cache,&key) {
        eprintln!("reusing native fine geometry cache"); items
    } else {
        let scene:crate::scene::Scene=serde_json::from_slice(&source_bytes).unwrap();
        let mut lib=crate::mesh::MeshLib::new(); lib.show_fine=true;
        eprintln!("building copied mech scene"); lib.sync(&scene);
        assert!(lib.fine_errors().is_empty(),"fine mesh errors: {:?}",lib.fine_errors());
        let eval=scene.evaluate();
        let draws=crate::envkit::scatter::draws(&scene,&eval);
        let items:Vec<Item>=draws.iter().filter(|d| d.object.shape.is_mesh()).filter_map(|d|Some(Item{mesh:lib.get(d.object)?.clone(),world:d.world,tint:d.object.color,surface:scene.surface_of(d.object),material:scene.material_of(d.object)})).collect();
        write_look_geometry(&cache,&key,&items);
        items
    };
    // Stage the existing soldier close to the telephoto camera, as a trailer
    // crew would. His original position cannot enter a 200 mm shoulder crop.
    // Keep the source scene untouched and save the exact staged scene for MCP.
    let mut staged:crate::scene::Scene=serde_json::from_slice(&source_bytes).unwrap();
    let original_eval=staged.evaluate();
    let original_draws=crate::envkit::scatter::draws(&staged,&original_eval);
    let ids:Vec<_>=original_draws.iter().filter(|d|d.object.shape.is_mesh()).map(|d|d.object.id).collect();
    assert_eq!(ids.len(),items.len());
    let soldier=staged.objects.iter().find(|o|o.name=="Soldier").map(|o|o.id).expect("Soldier");
    let mut foreground=staged.descendants(soldier); foreground.push(soldier);
    let delta=Vec3::new(11.65,0.0,16.67);
    let staged_items=items.iter().zip(&ids).map(|(item,id)|Item {
        mesh:item.mesh.clone(),world:if foreground.contains(id) {Mat4::from_translation(delta)*item.world} else {item.world},
        tint:item.tint,surface:item.surface,material:item.material.clone()
    }).collect();
    staged.get_mut(soldier).unwrap().translation+=delta;
    std::fs::write(output.join("telephoto_scene.json"),serde_json::to_vec(&staged).unwrap()).unwrap();
    let telephoto_meshes=Meshes::new(staged_items);
    let meshes=Meshes::new(items);
    let width=std::env::var("LOOK_SIZE").ok().and_then(|s|s.parse().ok()).unwrap_or(960);
    let height=width*9/16;
    let samples=std::env::var("LOOK_SAMPLES").ok().and_then(|s|s.parse().ok()).unwrap_or(128);
    let settings=Settings{width,height,samples,bounces:4,time_limit:Duration::from_secs(600),floor:false,exposure:1.0,denoise:true};
    let wide=Camera{eye:Vec3::new(9.0,2.0,15.0),target:Vec3::new(0.0,3.4,0.0),up:Vec3::Y,fov_deg:30.0,ortho:None};
    // Full-frame 200 mm at ~25 m, centred on cockpit/shoulder.
    let detail=Camera{eye:Vec3::new(17.0,1.2,20.0),target:Vec3::new(0.5,5.65,0.5),up:Vec3::Y,fov_deg:(24.0f32/400.0).atan().to_degrees()*2.0,ortho:None};
    let trailer=json!({"look":"trailer","environment":{"preset":"storm","intensity":0.65},"camera":{"position":wide.eye.to_array(),"target":wide.target.to_array(),"fov":30,"distortion":-0.025,"chromatic_aberration":0.001},"lights":[{"type":"sun","direction":[-0.5,0.4,-0.8],"color":[1,0.75,0.43],"intensity":5,"size":1},{"type":"sun","direction":[0.4,0.8,0.8],"color":[0.48,0.65,1],"intensity":0.7,"size":15}],"fog":{"density":0.055,"height":0.1,"falloff":0.8,"distance":40,"noise_scale":0.45,"noise":0.85,"anisotropy":0.45}});
    let detail_after=json!({"look":"trailer","environment":{"preset":"golden_hour","intensity":0.2},"lights":[{"type":"sun","direction":[0.5,0.7,0.8],"color":[1,0.78,0.5],"intensity":3,"size":3},{"type":"sun","direction":[-0.6,0.2,-0.8],"color":[1,0.65,0.3],"intensity":8,"size":1}],"fog":{"density":0.006,"height":0.2,"falloff":1.5,"distance":90,"noise":0.65,"noise_scale":0.18,"color":[0.95,0.83,0.65]},"camera":{"position":detail.eye.to_array(),"target":detail.target.to_array(),"focal_length_mm":200,"sensor_size_mm":[36,24],"aperture":2.8,"dof":{"focus_point":[0.3,6.15,0.8],"blades":7}},"grain":0.006});
    // Low, nearly backlit sun: real geometry occludes single scattering.
    let shafts=json!({"look":"trailer","environment":{"preset":"storm","intensity":0.12},"camera":{"position":wide.eye.to_array(),"target":wide.target.to_array(),"fov":30,"aperture":null},"lights":[{"type":"sun","direction":[-0.36,0.16,-0.92],"color":[1,0.65,0.3],"intensity":12,"size":0.35},{"type":"sun","direction":[0.4,0.8,0.8],"color":[0.48,0.65,1],"intensity":0.25,"size":15}],"fog":{"density":0.14,"height":0.3,"falloff":3.5,"distance":65,"noise_scale":0.22,"noise":0.9,"anisotropy":0.65,"color":[0.95,0.83,0.65]},"grain":0.006});
    let cases=[
        ("trailer_before",wide,json!({"environment":"golden_hour","lights":"none","background":true})),
        ("trailer_after",wide,trailer),
        ("telephoto_before",detail,{let mut pinhole=detail_after.clone(); pinhole["camera"]["aperture"]=serde_json::Value::Null; pinhole}),
        ("telephoto_after",detail,detail_after),
        ("dust_shafts_control",wide,{let mut control=shafts.clone(); control["fog"]=json!(false); control}),
        ("dust_shafts",wide,shafts),
        ("night_ops_before",wide,json!({"environment":"night","background":true,"lights":[{"type":"sun","direction":[-0.6,0.5,-0.4],"color":[0.25,0.55,0.9],"intensity":1.5,"size":2}]})),
        ("night_ops_after",wide,json!({"look":"night_ops","camera":{"position":wide.eye.to_array(),"target":wide.target.to_array(),"fov":30},"fog":{"density":0.03,"height":0.2,"falloff":1.2,"distance":35,"noise":0.7},"extra_lights":[{"type":"sphere","position":[2,4,6],"color":[0.18,0.5,1],"intensity":15,"radius":1}]})),
    ];
    let recipes:serde_json::Value=std::env::var("LOOK_RECIPES").map(|path|serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()).unwrap_or_default();
    let save=|path:std::path::PathBuf,pixels:&[[u8;3]],w:usize,h:usize| image::save_buffer(path,&pixels.iter().flatten().copied().collect::<Vec<_>>(),w as u32,h as u32,image::ColorType::Rgb8).unwrap();
    for (name,cam,args) in cases {
        if std::env::var("LOOK_CASE").is_ok_and(|v| !v.split(',').any(|s|s==name)) {continue;}
        let args=recipes.get(name).cloned().unwrap_or(args);
        let settings=Settings{exposure:args["exposure"].as_f64().unwrap_or(1.0) as f32,floor:args["floor"].as_bool().unwrap_or(false),denoise:args["denoise"].as_bool().unwrap_or(true),..settings};
        let look=Arc::new(Look::parse(&args).unwrap());
        let cam=look.camera.apply(cam).unwrap();
        let shot_meshes=if name.starts_with("telephoto_") {&telephoto_meshes} else {&meshes};
        let img=render_views_with_look(shot_meshes,&[],&[cam],settings,look,&Default::default()).pop().unwrap();
        eprintln!("{name}: {} spp, {:.1}s",img.samples,img.seconds);
        save(output.join(format!("{name}.png")),&img.pixels,width,height);
        let mut recipe=args.clone();
        recipe["camera"]["position"]=json!(cam.eye.to_array()); recipe["camera"]["target"]=json!(cam.target.to_array()); recipe["camera"]["fov"]=json!(cam.fov_deg);
        recipe["size"]=json!(width); recipe["aspect"]=json!(width as f64/height as f64); recipe["samples"]=json!(img.samples);
        recipe["bounces"]=json!(settings.bounces); recipe["floor"]=json!(settings.floor); recipe["exposure"]=json!(settings.exposure); recipe["denoise"]=json!(settings.denoise); recipe["time_limit"]=json!(600);
        std::fs::write(output.join(format!("{name}.json")),serde_json::to_vec_pretty(&json!({"render_film":recipe,"resolved":super::look::resolved_args(&args).unwrap(),"samples":img.samples,"size":width,"height":height,"camera_position":cam.eye.to_array(),"camera_target":cam.target.to_array(),"fov":cam.fov_deg,"seconds":img.seconds,"scene":if name.starts_with("telephoto_") {"telephoto_scene.json"} else {"mech.json"}})).unwrap()).unwrap();
        if name=="trailer_after" {
            let names=["neutral","modern_warfare","desert_heat","night_ops","cinematic_teal_orange"];
            let mut sheet=vec![[12;3];width*3*height*2];
            for (k,name) in names.into_iter().enumerate() {
                let look=Look::parse(&json!({"grade":{"look":name},"tonemap":"agx","bloom":0.08,"vignette":0.15,"grain":0.008})).unwrap();
                let px=look.finish(img.linear.clone(),width,height,1.0);
                save(output.join(format!("grade_{name}.png")),&px,width,height);
                for y in 0..height { let dst=(k/3*height+y)*width*3+k%3*width; sheet[dst..dst+width].copy_from_slice(&px[y*width..(y+1)*width]); }
            }
            save(output.join("grading_contact_sheet.png"),&sheet,width*3,height*2);
        }
    }
}

/// Assemble labelled acceptance sheets without rebuilding or retracing geometry.
#[test]
#[ignore]
fn film_look_contact_sheets() {
    let output=std::path::PathBuf::from(std::env::var("LOOK_OUTPUT").unwrap_or_else(|_|"look_check".into()));
    // Tiny native bitmap captions keep the harness independent of OS fonts.
    fn glyph(c:char)->[u8;7] {
        match c {
            'A'=>[14,17,17,31,17,17,17], 'B'=>[30,17,17,30,17,17,30],
            'C'=>[14,17,16,16,16,17,14], 'D'=>[30,17,17,17,17,17,30],
            'E'=>[31,16,16,30,16,16,31], 'F'=>[31,16,16,30,16,16,16],
            'G'=>[14,17,16,23,17,17,14], 'H'=>[17,17,17,31,17,17,17],
            'I'=>[14,4,4,4,4,4,14], 'J'=>[7,2,2,2,18,18,12],
            'K'=>[17,18,20,24,20,18,17], 'L'=>[16,16,16,16,16,16,31],
            'M'=>[17,27,21,21,17,17,17], 'N'=>[17,25,21,19,17,17,17],
            'O'=>[14,17,17,17,17,17,14], 'P'=>[30,17,17,30,16,16,16],
            'Q'=>[14,17,17,17,21,18,13], 'R'=>[30,17,17,30,20,18,17],
            'S'=>[15,16,16,14,1,1,30], 'T'=>[31,4,4,4,4,4,4],
            'U'=>[17,17,17,17,17,17,14], 'V'=>[17,17,17,17,17,10,4],
            'W'=>[17,17,17,21,21,21,10], 'X'=>[17,17,10,4,10,17,17],
            'Y'=>[17,17,10,4,4,4,4], 'Z'=>[31,1,2,4,8,16,31],
            _=>[0;7]
        }
    }
    for (file,cols,names) in [
        ("grading_contact_sheet",3,vec!["grade_neutral","grade_modern_warfare","grade_desert_heat","grade_night_ops","grade_cinematic_teal_orange"]),
        ("before_after_contact_sheet",2,vec!["trailer_before","trailer_after","telephoto_before","telephoto_after","night_ops_before","night_ops_after","dust_shafts_control","dust_shafts"]),
        ("halo_before_after",2,vec!["trailer_halo_before","trailer_after","dust_shafts_halo_before","dust_shafts"])
    ] {
        let images:Vec<_>=names.iter().map(|name|image::open(output.join(format!("{name}.png"))).unwrap().to_rgb8()).collect();
        let w=images[0].width() as usize; let h=images[0].height() as usize; let footer=28;
        let sw=cols*w; let sh=names.len().div_ceil(cols)*(h+footer);
        let mut sheet=image::RgbImage::from_pixel(sw as u32,sh as u32,image::Rgb([16,19,22]));
        for (i,(name,img)) in names.iter().zip(images).enumerate() {
            let (ox,oy)=(i%cols*w,i/cols*(h+footer));
            image::imageops::replace(&mut sheet,&img,ox as i64,oy as i64);
            let text=name.trim_start_matches("grade_").replace('_'," ").to_uppercase();
            for (j,ch) in text.chars().enumerate() { for (y,row) in glyph(ch).into_iter().enumerate() { for x in 0..5 {
                if row & (1<<(4-x)) == 0 {continue;}
                for dy in 0..2 {for dx in 0..2 {
                    let px=ox+12+j*12+x*2+dx; let py=oy+h+7+y*2+dy;
                    if px<sw && py<sh {sheet.put_pixel(px as u32,py as u32,image::Rgb([210,219,224]));}
                }}
            } } }
        }
        sheet.save(output.join(format!("{file}.png"))).unwrap();
    }
}

// QA-only native mesh cache: vertex data, per-pixel shader materials and decals
// survive exactly (unlike a baked interchange export). Delete after mesh code edits.
fn write_look_geometry(path:&std::path::Path,key:&str,items:&[Item]) {
    use std::io::Write;
    fn blob(f:&mut impl Write,bytes:&[u8]) { f.write_all(&(bytes.len() as u64).to_le_bytes()).unwrap();f.write_all(bytes).unwrap(); }
    let mut file=std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    blob(&mut file,key.as_bytes()); file.write_all(&(items.len() as u64).to_le_bytes()).unwrap();
    for item in items {
        let metadata=json!({"world":item.world.to_cols_array(),"tint":item.tint,"surface":item.surface,"material":item.material,"materials":item.mesh.materials,"decals":*item.mesh.decals,"textures":item.mesh.textures.as_deref(),"imported":item.mesh.imported.as_deref(),"face":item.mesh.face.as_deref()});
        blob(&mut file,&serde_json::to_vec(&metadata).unwrap());
        blob(&mut file,bytemuck::cast_slice(&item.mesh.vertices));
        blob(&mut file,bytemuck::cast_slice(&item.mesh.indices));
    }
    file.flush().unwrap();
}
fn read_look_geometry(path:&std::path::Path,key:&str)->Option<Vec<Item>> {
    use std::io::Read;
    fn count(f:&mut impl Read)->Option<usize> { let mut n=[0;8];f.read_exact(&mut n).ok()?;usize::try_from(u64::from_le_bytes(n)).ok() }
    fn blob(f:&mut impl Read)->Option<Vec<u8>> {let n=count(f)?;if n>2_147_483_648 {return None;}let mut b=vec![0;n];f.read_exact(&mut b).ok()?;Some(b)}
    let mut file=std::io::BufReader::new(std::fs::File::open(path).ok()?);
    if blob(&mut file)?!=key.as_bytes() {return None;}
    let n=count(&mut file)?;if n>10000 {return None;}
    let mut items=Vec::new();
    for _ in 0..n {
        let m:serde_json::Value=serde_json::from_slice(&blob(&mut file)?).ok()?;
        let v=blob(&mut file)?;let i=blob(&mut file)?;
        let stride=std::mem::size_of::<crate::mesh::Vertex>();
        if !v.len().is_multiple_of(stride) || !i.len().is_multiple_of(4) {return None;}
        let mesh=crate::mesh::Mesh{vertices:v.chunks_exact(stride).map(bytemuck::pod_read_unaligned).collect(),indices:i.chunks_exact(4).map(bytemuck::pod_read_unaligned).collect(),materials:serde_json::from_value(m["materials"].clone()).ok()?,decals:Arc::new(serde_json::from_value(m["decals"].clone()).ok()?),textures:serde_json::from_value::<Option<crate::mesh::Textures>>(m["textures"].clone()).ok()?.map(Arc::new),imported:serde_json::from_value::<Option<crate::mesh::ImportedMaterial>>(m["imported"].clone()).ok()?.map(Arc::new),face:serde_json::from_value::<Option<crate::face::Face>>(m["face"].clone()).ok()?.map(Arc::new),..Default::default()};
        items.push(Item{mesh:Arc::new(mesh),world:Mat4::from_cols_array(&serde_json::from_value(m["world"].clone()).ok()?),tint:serde_json::from_value(m["tint"].clone()).ok()?,surface:serde_json::from_value(m["surface"].clone()).ok()?,material:serde_json::from_value(m["material"].clone()).ok()?});
    }
    Some(items)
}

#[test]
fn look_cache_preserves_textured_meshes() {
    let path=std::env::temp_dir().join(format!("maya-look-cache-{}.cache",std::process::id()));
    let mesh=crate::mesh::Mesh {
        textures:Some(Arc::new(crate::mesh::Textures {size:1,color:vec![12,34,56,255],normal:vec![128,128,255,255],orm:vec![255,128,0,255],alpha:true})),
        imported:Some(Arc::new(crate::mesh::ImportedMaterial {emissive:[1.0,2.0,3.0],vertex_alpha:vec![0.5],..Default::default()})),
        ..Default::default()
    };
    let material=Material::default();
    let items=vec![Item {mesh:Arc::new(mesh),world:Mat4::IDENTITY,tint:[1.0;3],surface:material.surface(),material}];
    write_look_geometry(&path,"test-v2",&items);
    let restored=read_look_geometry(&path,"test-v2").unwrap();
    assert!(read_look_geometry(&path,"wrong-version").is_none());
    std::fs::remove_file(path).unwrap();
    let texture=restored[0].mesh.textures.as_ref().unwrap();
    assert_eq!(texture.size,1);
    assert_eq!(texture.color,vec![12,34,56,255]);
    assert_eq!(texture.normal,vec![128,128,255,255]);
    assert_eq!(texture.orm,vec![255,128,0,255]);
    assert!(texture.alpha);
    let imported=restored[0].mesh.imported.as_ref().unwrap();
    assert_eq!(imported.emissive,[1.0,2.0,3.0]);
    assert_eq!(imported.vertex_alpha,vec![0.5]);
}
