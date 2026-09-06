using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;

namespace Aurify.Plugin;

/// <summary>Draws its own screen inside the client window, or writes into existing ones.</summary>
public enum PluginKind
{
    App,
    Plugin,
}

/// <summary>
/// Where a plugin may write into the client. The list is closed by construction: the
/// client renders only what it knows, so a plugin cannot declare a place of its own.
/// </summary>
public static class SlotTypes
{
    public const string AiTab = "ai-tab";
    /// <summary>The one slot every plugin must declare: it is the only place it can be removed from.</summary>
    public const string AppsCard = "apps-card";
    public const string SettingsEntry = "settings-entry";
    public const string MessageAction = "message-action";
}

/// <summary>Declarative components the client can draw. Closed for the same reason as slots.</summary>
public static class ComponentTypes
{
    public const string Text = "text";
    public const string List = "list";
    public const string Toggle = "toggle";
    public const string Button = "button";
    public const string Progress = "progress";
}

public sealed class PluginSlot
{
    [JsonPropertyName("type")] public required string Type { get; init; }
    public string? Screen { get; init; }
    public string? Operation { get; init; }
    public string? Label { get; init; }
    public string? Icon { get; init; }
}

/// <summary>One drawn thing on a screen. <see cref="Bind"/> is a path into the screen's model.</summary>
public sealed class PluginComponent
{
    [JsonPropertyName("type")] public required string Type { get; init; }
    public string? Bind { get; init; }
    public string? Label { get; init; }
    public string? Operation { get; init; }
    public JsonNode? Item { get; init; }
}

public sealed class PluginScreen
{
    public required string Id { get; init; }
    public required string Title { get; init; }
    /// <summary>Operation whose result is this screen's model.</summary>
    public required string ModelOperation { get; init; }
    public IReadOnlyList<PluginComponent> Components { get; init; } = [];
}

public sealed class PluginOperation
{
    public required string Name { get; init; }
    public string? Description { get; init; }
    public JsonNode? Args { get; init; }
}

public sealed class PluginResources
{
    public long? DiskBytes { get; init; }
    /// <summary><c>none</c>, <c>optional</c> or <c>required</c>.</summary>
    public string? Gpu { get; init; }
}

public sealed class PluginProbe
{
    public int TimeoutSeconds { get; init; } = 30;
}

public sealed class PluginUninstall
{
    public IReadOnlyList<string> RemovePaths { get; init; } = [];
}

public sealed class AppEntry
{
    public required string Url { get; init; }
}

/// <summary>
/// Everything the client reads before installing anything. Anything not declared here is
/// unavailable to the plugin at runtime.
/// </summary>
public sealed class PluginManifest
{
    public const int CurrentSchemaVersion = 1;

    public int SchemaVersion { get; init; } = CurrentSchemaVersion;
    /// <summary>Stable identifier, <c>^[a-z][a-z0-9-]{2,63}$</c>. Never changes across versions.</summary>
    public required string Id { get; init; }
    public required string Version { get; init; }
    public PluginKind Kind { get; init; } = PluginKind.Plugin;
    public required string Title { get; init; }
    public string? Summary { get; init; }
    public IReadOnlyList<PluginSlot> Slots { get; init; } = [];
    public IReadOnlyList<PluginScreen> Screens { get; init; } = [];
    public IReadOnlyList<PluginOperation> Operations { get; init; } = [];
    /// <summary>Data kinds the plugin will ask for through the client's permission popup.</summary>
    public IReadOnlyList<string> Permissions { get; init; } = [];
    public PluginResources Resources { get; init; } = new();
    public PluginProbe Probe { get; init; } = new();
    public PluginUninstall Uninstall { get; init; } = new();
    public AppEntry? Entry { get; init; }

    internal static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.CamelCase) },
    };

    public string ToJson() => JsonSerializer.Serialize(this, Json);

    /// <summary>
    /// The rules the client enforces, as messages a person can act on. Empty means valid.
    /// Runs the same code the client runs before install, so a manifest that passes here
    /// is not refused later.
    /// </summary>
    public IReadOnlyList<string> Validate() => ValidateJson(ToJson());

    public static IReadOnlyList<string> ValidateJson(string manifestJson)
    {
        using var arg = new Native.Utf8Arg(manifestJson);
        var errors = Native.TakeString(Native.aurify_plugin_manifest_validate(arg.Pointer));
        if (errors is null) return [];
        return JsonSerializer.Deserialize<string[]>(errors) ?? [errors];
    }
}
