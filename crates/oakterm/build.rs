use vergen_gitcl::{BuildBuilder, CargoBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:rustc-env=INSTALL_SOURCE={}",
        option_env!("INSTALL_SOURCE").unwrap_or("source")
    );
    println!(
        "cargo:rustc-env=RELEASE_CHANNEL={}",
        option_env!("RELEASE_CHANNEL").unwrap_or("dev")
    );

    let build = BuildBuilder::all_build()?;
    let cargo = CargoBuilder::all_cargo()?;
    let gitcl = GitclBuilder::all_git()?;

    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&gitcl)?
        .emit()?;

    Ok(())
}
