# Aurify plugin SDK

Write plugins for the [Aurify](https://aurify.online) client.

A plugin is **a description of screens plus a program on the person's machine**. The
client draws the screens with its own components — so theme, font and language reach
your screen on their own — and talks to your program directly over loopback. Your
program can call back into the Aurify platform on the person's behalf, within the
permissions they granted.

This repository is the contract for that, in three parts, plus a native library that
implements the parts a program has to get right.

## The three contracts

| Contract | File | What it settles |
|---|---|---|
| Manifest | [`contracts/manifest.schema.json`](contracts/manifest.schema.json) | What a plugin declares: where it appears, which screens it has, which operations it answers, which data it will ask for. Anything not declared is unavailable at runtime. |
| Local protocol | [`contracts/local-protocol.openapi.json`](contracts/local-protocol.openapi.json) | Three routes your process serves on `127.0.0.1`: health, manifest, operations. Every request carries a secret the client generated for this install. |
| Launch | [`crates/aurify-plugin-core/src/launch.rs`](crates/aurify-plugin-core/src/launch.rs) | What the client hands your process through the environment when it starts it: port, secret, platform address and token, data directory. |

The enforcing validator for the manifest is Rust code, not the JSON Schema: the same
rules run in your build, in the client before install, and in catalog review, so a
manifest that passes locally is not refused later.

## Where a plugin may appear

Four places, and the list is closed — the client renders only what it knows.

| Slot | What it is | Cost |
|---|---|---|
| `apps-card` | Your card in the Apps section: version, permissions, remove. **Required** — it is the only place a plugin can be removed from. | none |
| `ai-tab` | An icon in the AI section rail and a whole screen behind it. | none |
| `settings-entry` | A row in the client's settings. | The row carries your plugin's name: settings is the one place people expect only the client's own. |
| `message-action` | An action on a message: translate, summarize, redraw. | Access to the message text, so it appears only after the person granted the conversation permission explicitly. |

## Quick start (.NET)

```bash
cargo build --release -p aurify-plugin-ffi      # the native library
dotnet run --project samples/hello-plugin       # a plugin with one screen and two operations
```

```csharp
using Aurify.Plugin;

var manifest = new PluginManifest
{
    Id = "hello-plugin",
    Version = "0.1.0",
    Title = "Hello",
    Slots = [ new PluginSlot { Type = SlotTypes.AppsCard, Screen = "main" } ],
    Screens = [ new PluginScreen { Id = "main", Title = "Hello", ModelOperation = "main.model" } ],
    Operations = [ new PluginOperation { Name = "main.model" } ],
};

using var host = PluginHost.Start(manifest, (name, args) => name switch
{
    "main.model" => new JsonObject { ["status"] = "Ready" },
    _ => throw new PluginOperationException("OPERATION_UNKNOWN", name),
});
host.SetHealth(PluginHealth.Ready);
```

`PluginHost.Start` reads the launch environment, validates the manifest and binds the
local server. Run by hand — outside the client — it throws and says which variable is
missing: a plugin only makes sense inside the client, and saying so beats a process that
listens on nothing.

The binding reads the environment itself and hands the values to the native library
explicitly. That is not a convenience: on Unix the .NET runtime keeps its own copy of the
environment and never calls `setenv`, so a library reading the environment on its own
would not see what the managed side set. A binding in another language should do the
same — `aurify_plugin_host_start` takes the launch values as JSON, and only reads the
environment when given `NULL`.

## Layout

```
contracts/                      manifest schema, local protocol
crates/aurify-plugin-core/      manifest, launch context, local server, platform client
crates/aurify-plugin-ffi/       C ABI over the core, built as a shared library; include/aurify_plugin.h
bindings/dotnet/Aurify.Plugin/  the .NET binding over the C ABI
samples/hello-plugin/           the smallest plugin that does something
```

The core is Rust and ships as one shared library with a C ABI, so a binding for any
language is a thin wrapper: strings cross the boundary as UTF-8 JSON, and every string
goes back to the allocator that made it (`aurify_plugin_string_alloc` /
`aurify_plugin_string_free`). The .NET binding is the reference; it is what Aurify's own
local-AI plugin is built on.

## What a plugin must not do

* Draw over the client with its own code — screens are declared, and the client draws them.
* Reach data the person did not grant, or any other person's data at all.
* Talk to the network beyond the platform and the addresses declared in the manifest.
* Leave anything on disk that `uninstall.removePaths` does not name.

## Status

Early. The contract will move while Aurify's own plugin is being ported onto it; the
manifest `schemaVersion` will change when it does, and the validator will refuse the old
one rather than guess.

## License

Apache-2.0.
