use super::*;
use sandboxed_sh::model_catalog::{merge_snapshot, snapshot_diff, Snapshot};

fn load_dir(dir: &Path) -> Result<BTreeMap<String, Snapshot>> {
    let mut result = BTreeMap::new();
    for file in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = file?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let snapshot: Snapshot = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        snapshot
            .validate()
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        if path.file_name().and_then(|s| s.to_str()) != Some(snapshot.filename().as_str()) {
            bail!("snapshot filename must match {}", snapshot.filename());
        }
        if result.insert(snapshot.filename(), snapshot).is_some() {
            bail!("duplicate snapshot profile");
        }
    }
    if result.is_empty() {
        bail!("no snapshots in {}", dir.display());
    }
    Ok(result)
}
fn output_dir(dir: &Path, snapshots: impl IntoIterator<Item = Snapshot>) -> Result<usize> {
    let snapshots: Vec<_> = snapshots.into_iter().collect();
    if snapshots.is_empty() {
        bail!("no successful exportable discoveries; no files changed");
    }
    for s in &snapshots {
        s.validate().map_err(|e| anyhow!(e))?;
    }
    fs::create_dir_all(dir)?;
    for snapshot in &snapshots {
        let path = dir.join(snapshot.filename());
        let temp = dir.join(format!(
            ".{}.{}.tmp",
            snapshot.filename(),
            uuid::Uuid::new_v4()
        ));
        fs::write(
            &temp,
            format!("{}\n", serde_json::to_string_pretty(snapshot)?),
        )?;
        fs::rename(&temp, &path)?;
    }
    Ok(snapshots.len())
}
pub fn run(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    if ["discover", "export"].contains(&command) {
        let api = value_after_flag(args, "--api")
            .map(str::to_owned)
            .or_else(|| env::var("SANDBOXED_API_URL").ok())
            .ok_or_else(|| anyhow!("missing --api or SANDBOXED_API_URL"))?;
        let token = api_token(args);
        let request = |method: &str, path: &str| -> Result<Value> {
            let url = format!("{}{}", api.trim_end_matches('/'), path);
            let mut call = ureq::request(method, &url).timeout(std::time::Duration::from_secs(300));
            if let Some(token) = token.as_deref() {
                call = call.set("Authorization", &format!("Bearer {token}"));
            }
            let (status, value) = match call.call() {
                Ok(response) => (response.status(), response.into_json::<Value>()?),
                Err(ureq::Error::Status(status, _)) => (status, Value::Null),
                Err(_) => bail!("catalog API transport error"),
            };
            if !(200..300).contains(&status) {
                bail!("catalog API returned HTTP {status}");
            }
            Ok(value)
        };
        if command == "discover" {
            request("POST", "/api/providers/catalog/refresh")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&request("GET", "/api/providers/discovery")?)?
            );
        } else {
            if value_after_flag(args, "--source").is_some_and(|s| s != "last-success") {
                bail!("only --source last-success is supported");
            }
            let snapshots: Vec<Snapshot> =
                serde_json::from_value(request("GET", "/api/providers/snapshots")?)?;
            let output = value_after_flag(args, "--output")
                .ok_or_else(|| anyhow!("missing --output DIR"))?;
            let count = output_dir(Path::new(output), snapshots)?;
            println!("{}", json!({"written":count,"directory":output}));
        }
        return Ok(());
    }
    match command {
        "validate" => {
            let path=args.get(1).ok_or_else(||anyhow!("models validate DIR"))?;
            let snapshots=load_dir(Path::new(path))?;
            println!("{}",json!({"valid":true,"snapshots":snapshots.len()}));
        }
        "diff" => {
            let base=value_after_flag(args,"--snapshot-dir").unwrap_or("catalog/snapshots");
            let incoming=value_after_flag(args,"--from").or_else(||args.get(1).filter(|s|!s.starts_with("--")).map(String::as_str))
                .ok_or_else(||anyhow!("models diff --snapshot-dir DIR --from DIR"))?;
            let old=load_dir(Path::new(base))?; let new=load_dir(Path::new(incoming))?;
            let changes:Vec<_>=new.iter().map(|(name,s)| match old.get(name) {
                Some(previous)=>snapshot_diff(previous,s),
                None=>json!({"file":name,"new_profile":true,"models":s.models.len()})
            }).collect();
            println!("{}",serde_json::to_string_pretty(&json!({"changes":changes,"missing_profiles_are_preserved":true}))?);
        }
        "snapshot" if args.get(1).map(String::as_str)==Some("update") => {
            let from=value_after_flag(args,"--from").ok_or_else(||anyhow!("missing --from DIR"))?;
            let dest=value_after_flag(args,"--snapshot-dir").unwrap_or("catalog/snapshots");
            let old=load_dir(Path::new(dest))?; let new=load_dir(Path::new(from))?;
            let mut prepared=Vec::new();
            for (name,incoming) in new {
                prepared.push(match old.get(&name) { Some(previous)=>merge_snapshot(previous,&incoming).map_err(|e|anyhow!(e))?,None=>incoming });
            }
            let count=output_dir(Path::new(dest),prepared)?;
            println!("{}",json!({"updated":count,"directory":dest}));
        }
        _ => bail!("models: discover --connected [--api URL] | export --source last-success --output DIR | validate DIR | diff --snapshot-dir DIR --from DIR | snapshot update --from DIR [--snapshot-dir DIR]"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_export_does_not_modify_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sentinel"), "keep").unwrap();
        assert!(output_dir(dir.path(), vec![]).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("sentinel")).unwrap(),
            "keep"
        );
    }
    #[test]
    fn filenames_cannot_redirect_snapshot_writes() {
        let dir = tempfile::tempdir().unwrap();
        let s = sandboxed_sh::model_catalog::bundled_snapshots().remove(0);
        fs::write(
            dir.path().join("wrong.json"),
            serde_json::to_vec(&s).unwrap(),
        )
        .unwrap();
        assert!(load_dir(dir.path()).is_err());
    }
}
