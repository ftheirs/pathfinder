pub fn register_all_methods(module: &mut jsonrpsee::RpcModule<()>) -> anyhow::Result<()> {
    use anyhow::Context;

    module
        .register_method("pathfinder_version", |_, _| {
            Ok(env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"))
        })
        .with_context(|| format!("Registering pathfinder_version"))?;

    Ok(())
}
