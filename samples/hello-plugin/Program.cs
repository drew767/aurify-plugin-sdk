using System.Text.Json.Nodes;
using Aurify.Plugin;

// The smallest plugin that does something: one screen with a list and a button, two
// operations behind them. The client draws the screen; this process only answers.

const string DevPort = "43210";
const string DevSecret = "hello-plugin-dev-secret-0123";

// Inside the client the environment is already set. Run by hand for a look around,
// the sample sets a port and a secret of its own and says so — a plugin that silently
// invents them would hide the launch contract from the person reading this file.
if (Environment.GetEnvironmentVariable("AURIFY_PLUGIN_PORT") is null)
{
    Environment.SetEnvironmentVariable("AURIFY_PLUGIN_PORT", DevPort);
    Environment.SetEnvironmentVariable("AURIFY_PLUGIN_SECRET", DevSecret);
    Console.WriteLine($"Not started by the client: listening on {DevPort} with secret {DevSecret}");
}

var manifest = new PluginManifest
{
    Id = "hello-plugin",
    Version = "0.1.0",
    Title = "Hello",
    Summary = "Greets by name and keeps a short list of who was greeted.",
    // The file in the published package the client starts, and what runs it.
    Entry = new ManifestEntry { Program = "hello-plugin.dll", Runtime = Runtimes.Dotnet },
    Slots =
    [
        new PluginSlot { Type = SlotTypes.AppsCard, Screen = "main" },
        new PluginSlot { Type = SlotTypes.AiTab, Screen = "main", Label = "Hello", Icon = "hand" },
    ],
    Screens =
    [
        new PluginScreen
        {
            Id = "main",
            Title = "Hello",
            ModelOperation = "main.model",
            Components =
            [
                new PluginComponent { Type = ComponentTypes.Text, Bind = "status" },
                new PluginComponent
                {
                    Type = ComponentTypes.List,
                    Bind = "greeted",
                    Item = new JsonObject { ["title"] = "name", ["subtitle"] = "at" },
                },
                new PluginComponent { Type = ComponentTypes.Button, Label = "Greet me", Operation = "greet" },
            ],
        },
    ],
    Operations =
    [
        new PluginOperation { Name = "main.model", Description = "Everyone greeted so far" },
        new PluginOperation
        {
            Name = "greet",
            Description = "Greet by name",
            Args = new JsonObject
            {
                ["type"] = "object",
                ["properties"] = new JsonObject { ["name"] = new JsonObject { ["type"] = "string" } },
            },
        },
    ],
    Permissions = ["identity.basic"],
    Resources = new PluginResources { DiskBytes = 1_000_000, Gpu = "none" },
};

var greeted = new List<(string Name, DateTimeOffset At)>();

using var host = PluginHost.Start(manifest, (name, args) =>
{
    switch (name)
    {
        case "main.model":
            return new JsonObject
            {
                ["status"] = greeted.Count == 0 ? "Nobody yet" : $"{greeted.Count} greeted",
                ["greeted"] = new JsonArray(greeted
                    .Select(g => (JsonNode)new JsonObject { ["name"] = g.Name, ["at"] = g.At.ToString("HH:mm") })
                    .ToArray()),
            };
        case "greet":
            var who = args["name"]?.GetValue<string>();
            if (string.IsNullOrWhiteSpace(who))
            {
                throw new PluginOperationException("NAME_REQUIRED", "Say who to greet.");
            }
            greeted.Add((who, DateTimeOffset.Now));
            return new JsonObject { ["text"] = $"Hello, {who}!" };
        default:
            throw new PluginOperationException("OPERATION_UNKNOWN", $"No such operation: {name}");
    }
});

host.SetHealth(PluginHealth.Ready);
Console.WriteLine($"hello-plugin {manifest.Version} on native {PluginHost.NativeVersion}, port {host.Port}. Ctrl+C to stop.");

var stop = new ManualResetEventSlim();
Console.CancelKeyPress += (_, e) =>
{
    e.Cancel = true;
    stop.Set();
};
stop.Wait();
