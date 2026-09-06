using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Aurify.Plugin;

public enum PluginHealth
{
    Starting,
    Ready,
    Failed,
}

/// <summary>
/// Thrown by an operation handler to answer the client with a machine code instead of a
/// crash. Any other exception becomes <c>PLUGIN_INTERNAL</c> with its message.
/// </summary>
public sealed class PluginOperationException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

public sealed class PluginStartException(string message) : Exception(message);

public sealed class PlatformCallException(string message) : Exception(message);

/// <summary>
/// What the client hands the process when it starts it. Read here, in managed code,
/// and passed to the native library explicitly: on Unix the .NET runtime keeps its own
/// copy of the environment and never calls <c>setenv</c>, so a variable set from C#
/// would be invisible to the library if it read the environment itself.
/// </summary>
public sealed class PluginLaunch
{
    public const string PortVariable = "AURIFY_PLUGIN_PORT";
    public const string SecretVariable = "AURIFY_PLUGIN_SECRET";
    public const string PlatformUrlVariable = "AURIFY_PLATFORM_URL";
    public const string PlatformTokenVariable = "AURIFY_PLATFORM_TOKEN";
    public const string DataDirVariable = "AURIFY_PLUGIN_DATA_DIR";

    public string? Port { get; init; }
    public string? Secret { get; init; }
    public string? PlatformUrl { get; init; }
    public string? PlatformToken { get; init; }
    public string? DataDir { get; init; }

    public static PluginLaunch FromEnvironment() => new()
    {
        Port = Environment.GetEnvironmentVariable(PortVariable),
        Secret = Environment.GetEnvironmentVariable(SecretVariable),
        PlatformUrl = Environment.GetEnvironmentVariable(PlatformUrlVariable),
        PlatformToken = Environment.GetEnvironmentVariable(PlatformTokenVariable),
        DataDir = Environment.GetEnvironmentVariable(DataDirVariable),
    };

    internal string ToJson() => new JsonObject
    {
        ["port"] = Port,
        ["secret"] = Secret,
        ["platformUrl"] = PlatformUrl,
        ["platformToken"] = PlatformToken,
        ["dataDir"] = DataDir,
    }.ToJsonString();
}

/// <summary>
/// A running plugin: the local server the client talks to, and the platform client for
/// calls made on the person's behalf.
/// </summary>
public sealed class PluginHost : IDisposable
{
    /// <summary>
    /// Runs one declared operation. Called on the server thread, so a slow handler
    /// blocks the next request: start long work and report it through a model.
    /// </summary>
    public delegate JsonNode? OperationHandler(string name, JsonNode args);

    // One trampoline for every host, kept in a static so the delegate the native side
    // holds is never collected. The host itself rides in user_data as a GC handle.
    private static readonly Native.OperationFn Trampoline = OnOperation;

    private readonly OperationHandler _handler;
    // Not readonly on purpose: GCHandle is a struct, and Free() on a readonly field runs
    // on a copy, leaving the field claiming to be allocated after the handle is gone.
    private GCHandle _self;
    private IntPtr _host;

    private PluginHost(OperationHandler handler)
    {
        _handler = handler;
        _self = GCHandle.Alloc(this);
    }

    /// <summary>Version of the native library, to compare with the binding's own.</summary>
    public static string NativeVersion => Native.ReadStatic(Native.aurify_plugin_version());

    /// <summary>
    /// Reads the launch environment the client set, validates the manifest and binds the
    /// local server. Throws when started by hand: the plugin only makes sense inside the
    /// client, and saying so beats a process that listens on nothing.
    /// </summary>
    public static PluginHost Start(PluginManifest manifest, OperationHandler handler) =>
        Start(manifest, PluginLaunch.FromEnvironment(), handler);

    /// <summary>Same, with the launch values given explicitly instead of read from the environment.</summary>
    public static PluginHost Start(PluginManifest manifest, PluginLaunch launch, OperationHandler handler)
    {
        ArgumentNullException.ThrowIfNull(manifest);
        ArgumentNullException.ThrowIfNull(launch);
        ArgumentNullException.ThrowIfNull(handler);

        var host = new PluginHost(handler);
        using var manifestArg = new Native.Utf8Arg(manifest.ToJson());
        using var launchArg = new Native.Utf8Arg(launch.ToJson());
        var pointer = Native.aurify_plugin_host_start(
            manifestArg.Pointer, launchArg.Pointer, Trampoline, GCHandle.ToIntPtr(host._self), out var errorOut);
        if (pointer == IntPtr.Zero)
        {
            host._self.Free();
            host._self = default;
            throw new PluginStartException(Native.TakeString(errorOut) ?? "the native library refused to start");
        }
        host._host = pointer;
        return host;
    }

    public int Port => Native.aurify_plugin_host_port(_host);

    /// <summary>Whether the client handed this process platform access.</summary>
    public bool HasPlatform => Native.aurify_plugin_host_has_platform(_host);

    /// <summary>
    /// The plugin decides when it is ready; the client only asks. Report
    /// <see cref="PluginHealth.Ready"/> once whatever the plugin needs is loaded.
    /// </summary>
    public void SetHealth(PluginHealth status, string? message = null)
    {
        using var statusArg = new Native.Utf8Arg(status.ToString().ToLowerInvariant());
        using var messageArg = new Native.Utf8Arg(message);
        Native.aurify_plugin_host_set_health(_host, statusArg.Pointer, messageArg.Pointer);
    }

    /// <summary>
    /// One call to the platform on the person's behalf, within the permissions they
    /// granted. <paramref name="path"/> is absolute, e.g. <c>/api/v1/generation/jobs</c>.
    /// </summary>
    public JsonNode? CallPlatform(string method, string path, JsonNode? body = null)
    {
        using var methodArg = new Native.Utf8Arg(method);
        using var pathArg = new Native.Utf8Arg(path);
        using var bodyArg = new Native.Utf8Arg(body?.ToJsonString());
        var answer = Native.aurify_plugin_platform_call(_host, methodArg.Pointer, pathArg.Pointer, bodyArg.Pointer, out var errorOut);
        if (answer == IntPtr.Zero)
        {
            throw new PlatformCallException(Native.TakeString(errorOut) ?? "the platform call failed");
        }
        var text = Native.TakeString(answer);
        return text is null ? null : JsonNode.Parse(text);
    }

    public void Dispose()
    {
        if (_host != IntPtr.Zero)
        {
            Native.aurify_plugin_host_stop(_host);
            _host = IntPtr.Zero;
        }
        if (_self.IsAllocated)
        {
            _self.Free();
            _self = default;
        }
        GC.SuppressFinalize(this);
    }

    private static IntPtr OnOperation(IntPtr namePointer, IntPtr argsPointer, IntPtr userData)
    {
        // Nothing may escape across the native boundary: an exception here would unwind
        // into Rust frames, and the process would die instead of the operation failing.
        try
        {
            var host = (PluginHost?)GCHandle.FromIntPtr(userData).Target;
            var name = Marshal.PtrToStringUTF8(namePointer) ?? string.Empty;
            var args = JsonNode.Parse(Marshal.PtrToStringUTF8(argsPointer) ?? "{}") ?? new JsonObject();
            if (host is null)
            {
                return Native.AllocForLibrary(Envelope.Error("PLUGIN_INTERNAL", "the host was already disposed"));
            }
            var result = host._handler(name, args);
            return Native.AllocForLibrary(Envelope.Result(result));
        }
        catch (PluginOperationException known)
        {
            return Native.AllocForLibrary(Envelope.Error(known.Code, known.Message));
        }
        catch (Exception unexpected)
        {
            return Native.AllocForLibrary(Envelope.Error("PLUGIN_INTERNAL", unexpected.Message));
        }
    }

    private static class Envelope
    {
        public static string Result(JsonNode? result) =>
            new JsonObject { ["result"] = result?.DeepClone() }.ToJsonString();

        public static string Error(string code, string message) =>
            new JsonObject { ["error"] = new JsonObject { ["code"] = code, ["message"] = message } }.ToJsonString();
    }
}
