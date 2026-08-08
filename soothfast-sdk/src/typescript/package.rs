//! Render the package scaffolding: `package.json`, `tsconfig.json`,
//! `README.md`, `src/index.ts`.

use std::fmt::Write;

use soothfast_spec::dialect::Info;

use crate::SdkOptions;
use crate::model::Sdk;
use crate::target::Target;
use crate::typescript::{HEADER, naming};

/// The npm package carrying one platform's binary.
pub(crate) fn platform_package(opts: &SdkOptions, target: &Target) -> String {
    format!("{}-{}", opts.package, target.npm_suffix())
}

/// One platform package's manifest. The binary itself is staged in by
/// `cargo soothfast sdk build`, not emitted here.
pub(crate) fn platform_package_json(opts: &SdkOptions, target: &Target, binary: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(
        out,
        "  \"name\": {},",
        naming::string_lit(&platform_package(opts, target))
    );
    let _ = writeln!(out, "  \"version\": {},", naming::string_lit(&opts.version));
    let _ = writeln!(
        out,
        "  \"description\": {},",
        naming::string_lit(&format!(
            "The {binary} binary for {}, used by {}.",
            target.npm_suffix(),
            opts.package
        ))
    );
    let _ = writeln!(out, "  \"os\": [{}],", naming::string_lit(target.os));
    let _ = writeln!(out, "  \"cpu\": [{}],", naming::string_lit(target.cpu));
    if let Some(libc) = target.libc {
        let _ = writeln!(out, "  \"libc\": [{}],", naming::string_lit(libc));
    }
    let _ = writeln!(out, "  \"files\": [");
    let _ = writeln!(out, "    \"bin\"");
    let _ = writeln!(out, "  ],");
    // Yarn PnP would otherwise leave the binary inside a zip, where it
    // cannot be spawned.
    let _ = writeln!(out, "  \"preferUnplugged\": true");
    let _ = writeln!(out, "}}");
    out
}

/// Runtime names re-exported from every generated package.
const RUNTIME_EXPORTS: &[&str] = &[
    "ApiError",
    "AsyncPager",
    "BadRequestError",
    "NotFoundError",
    "RateLimitError",
    "ServerError",
    "ServiceUnavailableError",
    "Transport",
    "type ApiErrorInit",
    "type BaseUrl",
    "type ClientOptions",
    "type Page",
    "type PageInfo",
    "type RequestOptions",
    "type TransportOptions",
];

/// `package.json` is plain JSON — no comment can mark it generated, so the
/// staleness check in `sdk gen --check` is what keeps it honest.
pub(crate) fn package_json(info: &Info, opts: &SdkOptions) -> String {
    let description = opts
        .description
        .clone()
        .or_else(|| info.description.clone())
        .unwrap_or_else(|| format!("Generated client for {}", info.title));

    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"name\": {},", naming::string_lit(&opts.package));
    let _ = writeln!(out, "  \"version\": {},", naming::string_lit(&opts.version));
    let _ = writeln!(
        out,
        "  \"description\": {},",
        naming::string_lit(&description)
    );
    let _ = writeln!(out, "  \"type\": \"module\",");
    let _ = writeln!(out, "  \"main\": \"./dist/index.js\",");
    let _ = writeln!(out, "  \"types\": \"./dist/index.d.ts\",");
    let _ = writeln!(out, "  \"exports\": {{");
    let _ = writeln!(out, "    \".\": {{");
    let _ = writeln!(out, "      \"types\": \"./dist/index.d.ts\",");
    let _ = writeln!(out, "      \"default\": \"./dist/index.js\"");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "  }},");
    let _ = writeln!(out, "  \"files\": [");
    let _ = writeln!(out, "    \"dist\",");
    let _ = writeln!(out, "    \"src\"");
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  \"engines\": {{");
    let _ = writeln!(out, "    \"node\": \">=18\"");
    let _ = writeln!(out, "  }},");
    let _ = writeln!(out, "  \"scripts\": {{");
    let _ = writeln!(out, "    \"build\": \"tsc -p tsconfig.json\",");
    let _ = writeln!(out, "    \"prepublishOnly\": \"npm run build\"");
    let _ = writeln!(out, "  }},");
    if let Some(repo) = &opts.repository {
        let _ = writeln!(out, "  \"repository\": {{");
        let _ = writeln!(out, "    \"type\": \"git\",");
        let _ = writeln!(out, "    \"url\": {}", naming::string_lit(repo));
        let _ = writeln!(out, "  }},");
    }
    // The binaries ride in as optional deps: npm installs only the one
    // matching `os`/`cpu`, and skips them all on an unlisted platform
    // rather than failing the install.
    if opts.embed.is_some() {
        let targets = Target::matrix(&opts.targets).unwrap_or_default();
        // Sorted by package name, the order every npm tool rewrites a
        // dependency map into.
        let mut names: Vec<String> = targets.iter().map(|t| platform_package(opts, t)).collect();
        names.sort();
        let _ = writeln!(out, "  \"optionalDependencies\": {{");
        for (i, name) in names.iter().enumerate() {
            let comma = if i + 1 == names.len() { "" } else { "," };
            let _ = writeln!(
                out,
                "    {}: {}{comma}",
                naming::string_lit(name),
                naming::string_lit(&opts.version),
            );
        }
        let _ = writeln!(out, "  }},");
    }
    let _ = writeln!(out, "  \"devDependencies\": {{");
    // The launcher spawns a subprocess, so an embedding package is
    // Node-only and needs Node's types. A pure client stays agnostic.
    if opts.embed.is_some() {
        let _ = writeln!(out, "    \"@types/node\": \"^22.0.0\",");
    }
    let _ = writeln!(out, "    \"typescript\": \"^5.6.0\"");
    let _ = writeln!(out, "  }}");
    let _ = writeln!(out, "}}");
    out
}

/// NodeNext resolution, so the emitted `.js` specifiers resolve in Node and
/// in every bundler without a compatibility shim.
pub(crate) fn tsconfig(opts: &SdkOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"compilerOptions\": {{");
    let _ = writeln!(out, "    \"target\": \"ES2022\",");
    let _ = writeln!(out, "    \"lib\": [\"ES2022\", \"DOM\"],");
    if opts.embed.is_some() {
        let _ = writeln!(out, "    \"types\": [\"node\"],");
    }
    let _ = writeln!(out, "    \"module\": \"NodeNext\",");
    let _ = writeln!(out, "    \"moduleResolution\": \"NodeNext\",");
    let _ = writeln!(out, "    \"strict\": true,");
    let _ = writeln!(out, "    \"declaration\": true,");
    let _ = writeln!(out, "    \"declarationMap\": true,");
    let _ = writeln!(out, "    \"sourceMap\": true,");
    let _ = writeln!(out, "    \"outDir\": \"dist\",");
    let _ = writeln!(out, "    \"rootDir\": \"src\",");
    let _ = writeln!(out, "    \"skipLibCheck\": true,");
    let _ = writeln!(out, "    \"forceConsistentCasingInFileNames\": true");
    let _ = writeln!(out, "  }},");
    let _ = writeln!(out, "  \"include\": [\"src\"]");
    let _ = writeln!(out, "}}");
    out
}

pub(crate) fn index(info: &Info, sdk: &Sdk, opts: &SdkOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(
        out,
        "/** {} — generated client for {}. */",
        opts.package, info.title
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "export * from \"./client.js\";");
    if !sdk.models.is_empty() || !sdk.aliases.is_empty() {
        let _ = writeln!(out, "export * from \"./models.js\";");
    }
    let _ = writeln!(out, "export {{");
    for name in RUNTIME_EXPORTS {
        let _ = writeln!(out, "  {name},");
    }
    let _ = writeln!(out, "}} from \"./runtime.js\";");
    if opts.embed.is_some() {
        let _ = writeln!(out, "export {{");
        for name in [
            "EmbeddedServer",
            "stopEmbeddedServers",
            "type EmbedConfig",
            "type StartOptions",
        ] {
            let _ = writeln!(out, "  {name},");
        }
        let _ = writeln!(out, "}} from \"./server.js\";");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "/** The version this client was generated for. */");
    let _ = writeln!(
        out,
        "export const VERSION = {};",
        naming::string_lit(&opts.version)
    );
    out
}

/// A copyable example: the knobs the template leaves unset are the ones a
/// caller actually has to supply, so lead with those.
fn example_env(opts: &SdkOptions) -> String {
    let unset: Vec<&crate::envtemplate::EnvVar> = opts
        .embed_env_vars
        .iter()
        .filter(|v| v.optional())
        .collect();
    let picks = if unset.is_empty() {
        opts.embed_env_vars.iter().collect()
    } else {
        unset
    };
    picks
        .iter()
        .take(2)
        .map(|v| format!("{}: \"...\"", v.name))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn readme(info: &Info, sdk: &Sdk, opts: &SdkOptions, base_url: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<!-- Code generated by `cargo soothfast sdk gen`. DO NOT EDIT. -->"
    );
    let _ = writeln!(out, "# {}", opts.package);
    let _ = writeln!(out);
    let _ = write!(
        out,
        "Generated TypeScript client for **{}** ({})",
        info.title, info.version
    );
    match (&opts.embed, base_url) {
        (Some(_), _) => {
            let _ = writeln!(
                out,
                ", bundling the server it talks to — there is nothing to \
                 deploy and no endpoint to configure."
            );
        }
        (None, Some(url)) => {
            let _ = writeln!(out, ", talking to `{url}`.");
        }
        (None, None) => {
            let _ = writeln!(out, ".");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "```bash");
    let _ = writeln!(out, "npm install {}", opts.package);
    let _ = writeln!(out, "```");
    let _ = writeln!(out);
    let _ = writeln!(out, "```ts");
    let _ = writeln!(
        out,
        "import {{ Client }} from {};",
        naming::string_lit(&opts.package)
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "const client = new Client();");
    match sdk.methods.first() {
        Some(m) => {
            let _ = writeln!(out, "const result = await client.{}(...);", m.name);
        }
        None => {
            let _ = writeln!(out, "// no operations");
        }
    }
    let _ = writeln!(out, "```");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Errors throw subclasses of `ApiError`; rate limits retry \
         automatically, honoring `Retry-After`. Model properties use the \
         wire names the API sends, so a decoded response needs no \
         translation step."
    );
    let _ = writeln!(out);
    if let Some(binary) = &opts.embed {
        let prefix = crate::naming::env_prefix(&opts.package);
        let _ = writeln!(
            out,
            "## Embedded server\n\n\
             The first request starts the bundled `{binary}` on a port it \
             picks itself, and the process is reaped when yours exits. Two \
             environment variables change that:\n\n\
             - `{prefix}_BASE_URL` — talk to an already-running instance \
             and spawn nothing.\n\
             - `{prefix}_SERVER_BIN` — use a different server binary.\n\n\
             `stopEmbeddedServers()` shuts it down early; \
             `EmbeddedServer.start()` manages one by hand."
        );
        if let Some(url) = base_url {
            let _ = writeln!(
                out,
                "\nPass `DEFAULT_BASE_URL` (`{url}`) to use the hosted \
                 deployment instead."
            );
        }
        if !opts.embed_env_vars.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "### Configuring it\n\n\
                 The server reads the same environment a deployment sets it \
                 with. `serverEnv` supplies that per client:\n\n\
                 ```ts\n\
                 const client = new Client(undefined, {{\n\
                 \x20 serverEnv: {{ {} }},\n\
                 }});\n\
                 ```\n\n\
                 Those values win over the ambient environment; anything left \
                 out falls back to what this process — or a `.env` file the \
                 server itself reads — already provides. Clients asking for \
                 different `serverEnv` get their own server process. The \
                 `ServerEnv` interface spells every knob:\n",
                example_env(opts),
            );
            let _ = write!(
                out,
                "{}",
                crate::envtemplate::markdown_table(&opts.embed_env_vars)
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Requires Node 18+.");
    } else {
        let _ = writeln!(
            out,
            "Requires Node 18+ (or any runtime with a global `fetch`)."
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Methods");
    let _ = writeln!(out);
    for m in &sdk.methods {
        let doc = m.doc.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "- `{}` — `{} {}`{}{}",
            m.name,
            m.http_method,
            m.path,
            if doc.is_empty() { "" } else { ": " },
            doc,
        );
    }
    out
}
