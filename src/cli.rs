use avt_replenishment::core::{execute, inspect};
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if args
        .get(1)
        .is_some_and(|a| a == "check-update" || a == "verify-update")
    {
        let update = avt_replenishment::updates::windows::check_latest()?;
        if args[1] == "verify-update" {
            let bytes = avt_replenishment::updates::windows::download_update(&update)?;
            let exe = avt_replenishment::updates::extract_program(&bytes)?;
            println!(
                "Public release download and SHA-256 verified: {} bytes, program {} bytes",
                bytes.len(),
                exe.len()
            );
        }
        println!(
            "current={} latest={} update_available={} release={}",
            avt_replenishment::updates::VERSION,
            update.latest,
            update.newer,
            update.release_url
        );
        return Ok(());
    }
    if args.len() < 3 {
        anyhow::bail!("Usage: avt-cli inspect <xlsx> | apply <xlsx> [--backup]");
    }
    let plan = inspect(std::path::Path::new(&args[2]))?;
    if args[1] == "inspect" {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else if args[1] == "apply" {
        println!(
            "{}",
            serde_json::to_string_pretty(&execute(&plan, args.iter().any(|s| s == "--backup"))?)?
        );
    } else {
        anyhow::bail!("Unknown command");
    }
    Ok(())
}
