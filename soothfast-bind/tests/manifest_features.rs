//! The features a surface was walked with reach every generated manifest's
//! dependency on the bound crate, so the crate the glue compiles against is
//! the one rustdoc saw.

mod fixture;

use fixture::{
    cpp_opts, csharp_opts, go_opts, java_opts, kotlin_opts, lua_opts, opts, r_opts, ruby_opts, walk,
};
use soothfast_bind::{BindKind, BindOptions};

fn opts_for(kind: BindKind) -> BindOptions {
    let base = match kind {
        BindKind::Go => go_opts(),
        BindKind::Java => java_opts(),
        BindKind::Kotlin => kotlin_opts(),
        BindKind::R => r_opts(),
        BindKind::Ruby => ruby_opts(),
        BindKind::Cpp => cpp_opts(),
        BindKind::Lua => lua_opts(),
        BindKind::CSharp => csharp_opts(),
        _ => opts(),
    };
    BindOptions {
        features: vec!["indicators".into()],
        ..base
    }
}

#[test]
fn every_manifest_depends_on_the_bound_crate_with_the_walked_features() {
    let (surface, gaps) = walk();
    for &kind in BindKind::ALL {
        let opts = opts_for(kind);
        let files = kind
            .emit(&surface, gaps.clone(), &opts)
            .expect("emits")
            .files;
        let manifests: Vec<(&String, &String)> = files
            .iter()
            .filter(|(name, text)| {
                name.ends_with("Cargo.toml")
                    && text.contains(&format!("\n{} = {{", opts.crate_package))
            })
            .collect();
        assert!(
            !manifests.is_empty(),
            "{}: no manifest names the bound crate",
            kind.name()
        );
        for (name, text) in manifests {
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!("{} = {{", opts.crate_package)))
                .expect("dependency line");
            assert!(
                line.contains("features = [\"indicators\"]"),
                "{}: {name}: {line}",
                kind.name()
            );
        }
    }
}
