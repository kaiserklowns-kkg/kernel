//! `oceans`: the Oceans developer tool (ADR-0062).
//!
//! ```text
//! oceans new rust|go ID [DIR] [--name NAME] [--publisher NAME]
//! oceans keygen PUBLISHER [--out FILE]
//! oceans build [DIR] --key FILE
//! oceans trust --key FILE
//! oceans serve [DIR] [--port PORT]
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use oceans_dev::{DeveloperKey, Project, Template, package, package_name, sdk_root, store_index};

const USAGE: &str = "\
usage:
  oceans new rust|go|sveltekit ID [DIR] [--name NAME] [--publisher NAME]
      a new app from a template (DIR defaults to the id's last part)
  oceans keygen PUBLISHER [--out FILE]
      a developer key signing as PUBLISHER (default file: oceans-developer.key)
  oceans build [DIR] --key FILE
      builds the app in DIR and signs its package into DIR/dist/
  oceans trust --key FILE [--until YYYY-MM-DD]
      the command that makes Oceans trust the key (run it in the Oceans shell),
      through a date if given
  oceans serve [DIR] [--port PORT]
      serves DIR/dist/ as a store (index.json and packages) on PORT (8000)

environment: OCEANS_SDK (the SDK's root; default: where this tool was built),
OCEANS_GO (the Go command, default `go`), OCEANS_BUN (Bun, default `bun`)";

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("oceans: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--flag VALUE` options out of `args`; the rest stay positional.
type Options<'a> = (Vec<&'a str>, Vec<(&'static str, &'a str)>);

fn options<'a>(args: &'a [String], flags: &[&'static str]) -> Result<Options<'a>> {
    let mut positional = Vec::new();
    let mut found = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(flag) = flags.iter().find(|f| **f == arg) {
            let value = it.next().ok_or(format!("{flag} needs a value"))?;
            found.push((*flag, value.as_str()));
        } else if arg.starts_with("--") {
            return Err(format!("unknown option {arg}\n{USAGE}"));
        } else {
            positional.push(arg.as_str());
        }
    }
    Ok((positional, found))
}

fn option<'a>(found: &[(&str, &'a str)], flag: &str) -> Option<&'a str> {
    found.iter().find(|(f, _)| *f == flag).map(|(_, v)| *v)
}

fn run(args: &[String]) -> Result {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match command.as_str() {
        "new" => new(rest),
        "keygen" => keygen(rest),
        "build" => build(rest),
        "trust" => {
            let (_, found) = options(rest, &["--key", "--until"])?;
            let key = read_key(option(&found, "--key").ok_or("which key? (--key FILE)")?)?;
            match option(&found, "--until") {
                Some(day) if oceans_package::date::parse(day).is_none() => {
                    Err(format!("{day}: not a date (YYYY-MM-DD)"))
                }
                Some(day) => {
                    println!("{} --until {day}", key.trust_command());
                    Ok(())
                }
                None => {
                    println!("{}", key.trust_command());
                    Ok(())
                }
            }
        }
        "serve" => serve(rest),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn new(args: &[String]) -> Result {
    let (positional, found) = options(args, &["--name", "--publisher"])?;
    let (template, id, dir) = match positional.as_slice() {
        [template, id] => (*template, *id, None),
        [template, id, dir] => (*template, *id, Some(*dir)),
        _ => return Err(USAGE.into()),
    };
    let template = Template::from_name(template).ok_or("templates: rust, go, sveltekit")?;
    let last = id.rsplit('.').next().unwrap_or(id);
    let default_name = {
        let mut chars = last.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    let project = Project {
        id: id.to_string(),
        name: option(&found, "--name").map_or(default_name, str::to_string),
        publisher: option(&found, "--publisher")
            .unwrap_or("Example Developer")
            .to_string(),
        sdk: sdk_root(),
    };
    project.check()?;
    let dir = PathBuf::from(dir.unwrap_or(project.program()));
    if dir.exists() && fs::read_dir(&dir).map_or(true, |mut d| d.next().is_some()) {
        return Err(format!("{} exists and is not empty", dir.display()));
    }
    for (path, text) in template.files() {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&target, project.fill(text)).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    println!(
        "new {} app {} in {} (publisher: {})",
        match template {
            Template::Rust => "Rust",
            Template::Go => "Go",
            Template::SvelteKit => "SvelteKit",
        },
        project.id,
        dir.display(),
        project.publisher
    );
    println!("next: oceans build {} --key YOUR.key", dir.display());
    Ok(())
}

fn keygen(args: &[String]) -> Result {
    let (positional, found) = options(args, &["--out"])?;
    if positional.is_empty() {
        return Err("keygen needs the publisher's name".into());
    }
    let publisher = positional.join(" ");
    let out = Path::new(option(&found, "--out").unwrap_or("oceans-developer.key"));
    if out.exists() {
        return Err(format!("{} exists: not overwriting a key", out.display()));
    }
    let key = DeveloperKey::generate(&publisher)?;
    // Same checks as a project's publisher: it goes into manifests.
    Project {
        id: "app.example.key".into(),
        name: "key".into(),
        publisher: publisher.clone(),
        sdk: String::new(),
    }
    .check()?;
    fs::write(out, key.to_file()).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "a developer key for \"{publisher}\" is in {}: keep it secret",
        out.display()
    );
    println!("to let an Oceans system install your apps, run in its shell:");
    println!("  {}", key.trust_command());
    Ok(())
}

fn read_key(path: &str) -> Result<DeveloperKey> {
    let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    DeveloperKey::parse(&text).map_err(|e| format!("{path}: {e}"))
}

fn build(args: &[String]) -> Result {
    let (positional, found) = options(args, &["--key"])?;
    let dir = PathBuf::from(positional.first().copied().unwrap_or("."));
    let key = read_key(option(&found, "--key").ok_or("which key signs it? (--key FILE)")?)?;
    let manifest_text = fs::read_to_string(dir.join("manifest"))
        .map_err(|e| format!("{}: {e}", dir.join("manifest").display()))?;
    let manifest = oceans_package::Manifest::parse(&manifest_text)
        .map_err(|e| format!("manifest: {}", e.message()))?;
    let program = match manifest.runtime {
        oceans_package::Runtime::Native => {
            let program = manifest.entry;
            status(
                Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                    .args(["build", "--release", "--bin", program])
                    .current_dir(&dir),
            )?;
            dir.join("target/x86_64-unknown-none/release").join(program)
        }
        oceans_package::Runtime::Web => {
            let bun = std::env::var("OCEANS_BUN").unwrap_or_else(|_| "bun".into());
            status(
                Command::new(&bun)
                    .args(["install", "--frozen-lockfile"])
                    .current_dir(&dir),
            )?;
            status(Command::new(&bun).args(["run", "build"]).current_dir(&dir))?;
            let out = dir.join("build").join(manifest.entry);
            let bundle = oceans_dev::web_bundle(&dir.join("build"))?;
            fs::write(&out, bundle).map_err(|e| format!("{}: {e}", out.display()))?;
            out
        }
        oceans_package::Runtime::Wasm => {
            let out = dir.join("build").join(manifest.entry);
            status(
                Command::new(std::env::var("OCEANS_GO").unwrap_or_else(|_| "go".into()))
                    .args(["build", "-trimpath", "-o"])
                    .arg(Path::new("build").join(manifest.entry))
                    .arg(".")
                    .env("GOOS", "wasip1")
                    .env("GOARCH", "wasm")
                    .current_dir(&dir),
            )?;
            out
        }
    };
    let program = fs::read(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    let bytes = package(&manifest_text, &program, &key)?;
    let dist = dir.join("dist");
    fs::create_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
    let out = dist.join(package_name(&manifest_text)?);
    fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "{} {} signed for {}: {} ({} KiB)",
        manifest.id,
        manifest.version,
        key.publisher,
        out.display(),
        bytes.len() / 1024
    );
    Ok(())
}

fn status(command: &mut Command) -> Result {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed ({status})"))
    }
}

fn serve(args: &[String]) -> Result {
    let (positional, found) = options(args, &["--port"])?;
    let dist = PathBuf::from(positional.first().copied().unwrap_or(".")).join("dist");
    let port: u16 = option(&found, "--port")
        .unwrap_or("8000")
        .parse()
        .map_err(|_| "--port takes a number")?;
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("port {port}: {e}"))?;
    println!(
        "serving {} as a store: set http://THIS-MACHINE:{port} as the Store's URL in Oceans",
        dist.display()
    );
    for stream in listener.incoming().flatten() {
        let _ = answer(stream, &dist);
    }
    Ok(())
}

/// One request: `GET /index.json` or `GET /NAME.opk`.
fn answer(mut stream: TcpStream, dist: &Path) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let name = path.rsplit('/').next().unwrap_or("");
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.starts_with('.');
    let body = if name == "index.json" {
        let mut packages = Vec::new();
        for entry in fs::read_dir(dist)?.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.ends_with(".opk") {
                packages.push((file, fs::read(entry.path())?));
            }
        }
        packages.sort();
        Some(store_index(&packages).into_bytes())
    } else if plain && name.ends_with(".opk") {
        fs::read(dist.join(name)).ok()
    } else {
        None
    };
    let (status, body) = match body {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", b"not here\n".to_vec()),
    };
    println!(
        "{} {path} -> {status}",
        line.split_whitespace().next().unwrap_or("?")
    );
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)
}
