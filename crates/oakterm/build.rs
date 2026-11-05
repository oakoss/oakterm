use vergen_gitcl::{Build, Cargo, Emitter, Gitcl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:rustc-env=INSTALL_SOURCE={}",
        option_env!("INSTALL_SOURCE").unwrap_or("source")
    );
    println!(
        "cargo:rustc-env=RELEASE_CHANNEL={}",
        option_env!("RELEASE_CHANNEL").unwrap_or("dev")
    );

    let build = Build::all_build();
    let cargo = Cargo::all_cargo();
    let gitcl = Gitcl::all_git();

    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&gitcl)?
        .emit()?;

    Ok(())
}
