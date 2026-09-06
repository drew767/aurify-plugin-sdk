using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using Xunit;

namespace Aurify.Plugin.Tests;

public sealed class PluginHostTests
{
    private const string Secret = "test-secret-0123456789";

    private static PluginManifest Manifest() => new()
    {
        Id = "hello",
        Version = "1.0.0",
        Title = "Hello",
        Slots = [new PluginSlot { Type = SlotTypes.AppsCard, Screen = "main" }],
        Screens =
        [
            new PluginScreen
            {
                Id = "main",
                Title = "Main",
                ModelOperation = "main.model",
                Components = [new PluginComponent { Type = ComponentTypes.List, Bind = "items" }],
            },
        ],
        Operations = [new PluginOperation { Name = "main.model" }, new PluginOperation { Name = "greet" }],
    };

    private static int FreePort()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        return ((IPEndPoint)listener.LocalEndpoint).Port;
    }

    /// <summary>
    /// Sets the launch environment the client would set. Environment is process-wide, so
    /// tests that start a host run one at a time.
    /// </summary>
    private sealed class Launch : IDisposable
    {
        private static readonly SemaphoreSlim OneAtATime = new(1, 1);

        public int Port { get; }

        public Launch()
        {
            OneAtATime.Wait();
            Port = FreePort();
            Environment.SetEnvironmentVariable("AURIFY_PLUGIN_PORT", Port.ToString());
            Environment.SetEnvironmentVariable("AURIFY_PLUGIN_SECRET", Secret);
        }

        public void Dispose()
        {
            Environment.SetEnvironmentVariable("AURIFY_PLUGIN_PORT", null);
            Environment.SetEnvironmentVariable("AURIFY_PLUGIN_SECRET", null);
            OneAtATime.Release();
        }
    }

    private static HttpClient ClientFor(int port, string? secret = Secret)
    {
        var client = new HttpClient { BaseAddress = new Uri($"http://127.0.0.1:{port}/") };
        if (secret is not null) client.DefaultRequestHeaders.Add("X-Aurify-Plugin-Secret", secret);
        return client;
    }

    [Fact]
    public void The_native_library_reports_its_version()
    {
        Assert.Matches(@"^\d+\.\d+\.\d+", PluginHost.NativeVersion);
    }

    [Fact]
    public void Validation_runs_the_same_rules_the_client_enforces()
    {
        Assert.Empty(Manifest().Validate());

        var withoutCard = new PluginManifest { Id = "hello", Version = "1", Title = "Hello" };
        var errors = withoutCard.Validate();
        Assert.Contains(errors, e => e.Contains("apps-card"));

        var badJson = PluginManifest.ValidateJson("{not json");
        Assert.Contains(badJson, e => e.Contains("not valid JSON"));
    }

    [Fact]
    public void Starting_by_hand_says_so_instead_of_listening_on_nothing()
    {
        Environment.SetEnvironmentVariable("AURIFY_PLUGIN_PORT", null);
        var error = Assert.Throws<PluginStartException>(() => PluginHost.Start(Manifest(), (_, _) => null));
        Assert.Contains("AURIFY_PLUGIN_PORT", error.Message);
    }

    [Fact]
    public async Task Health_is_starting_until_the_plugin_says_otherwise()
    {
        using var launch = new Launch();
        using var host = PluginHost.Start(Manifest(), (_, _) => null);
        using var client = ClientFor(host.Port);

        var starting = await client.GetStringAsync("plugin/v1/health");
        Assert.Contains("\"starting\"", starting);

        host.SetHealth(PluginHealth.Ready, "models loaded");
        var ready = await client.GetStringAsync("plugin/v1/health");
        Assert.Contains("\"ready\"", ready);
        Assert.Contains("models loaded", ready);
    }

    [Fact]
    public async Task Operations_reach_the_handler_and_typed_failures_come_back_as_codes()
    {
        using var launch = new Launch();
        var seen = new List<string>();
        using var host = PluginHost.Start(Manifest(), (name, args) =>
        {
            seen.Add(name);
            return name switch
            {
                "main.model" => new JsonObject { ["items"] = new JsonArray("a", "b") },
                "greet" when args["name"] is JsonNode who => new JsonObject { ["text"] = $"hello, {who}" },
                "greet" => throw new PluginOperationException("NAME_REQUIRED", "say who to greet"),
                _ => throw new InvalidOperationException("undeclared operations never arrive"),
            };
        });
        using var client = ClientFor(host.Port);

        var model = await client.PostAsync("plugin/v1/operations/main.model", new StringContent("{}", Encoding.UTF8, "application/json"));
        Assert.Equal(HttpStatusCode.OK, model.StatusCode);
        Assert.Contains("[\"a\",\"b\"]", await model.Content.ReadAsStringAsync());

        var greeted = await client.PostAsync("plugin/v1/operations/greet",
            new StringContent("{\"args\":{\"name\":\"Ann\"}}", Encoding.UTF8, "application/json"));
        Assert.Contains("hello, Ann", await greeted.Content.ReadAsStringAsync());

        var refused = await client.PostAsync("plugin/v1/operations/greet", new StringContent("{}", Encoding.UTF8, "application/json"));
        Assert.Equal((HttpStatusCode)422, refused.StatusCode);
        Assert.Contains("NAME_REQUIRED", await refused.Content.ReadAsStringAsync());

        var unknown = await client.PostAsync("plugin/v1/operations/shutdown", new StringContent("{}", Encoding.UTF8, "application/json"));
        Assert.Equal(HttpStatusCode.NotFound, unknown.StatusCode);

        Assert.Equal(["main.model", "greet", "greet"], seen);
    }

    [Fact]
    public async Task An_unexpected_exception_in_a_handler_fails_the_operation_not_the_process()
    {
        using var launch = new Launch();
        using var host = PluginHost.Start(Manifest(), (_, _) => throw new InvalidOperationException("boom"));
        using var client = ClientFor(host.Port);

        var failed = await client.PostAsync("plugin/v1/operations/greet", new StringContent("{}", Encoding.UTF8, "application/json"));
        Assert.Equal((HttpStatusCode)422, failed.StatusCode);
        var body = await failed.Content.ReadAsStringAsync();
        Assert.Contains("PLUGIN_INTERNAL", body);
        Assert.Contains("boom", body);

        // The process is still alive and serving.
        Assert.Contains("\"starting\"", await client.GetStringAsync("plugin/v1/health"));
    }

    [Fact]
    public async Task Every_route_needs_the_secret()
    {
        using var launch = new Launch();
        using var host = PluginHost.Start(Manifest(), (_, _) => null);
        using var without = ClientFor(host.Port, secret: null);
        using var wrong = ClientFor(host.Port, secret: "wrong-wrong-wrong-wrong");

        Assert.Equal(HttpStatusCode.Unauthorized, (await without.GetAsync("plugin/v1/health")).StatusCode);
        Assert.Equal(HttpStatusCode.Unauthorized, (await wrong.GetAsync("plugin/v1/manifest")).StatusCode);
    }

    [Fact]
    public void Platform_access_is_absent_when_the_client_did_not_grant_it()
    {
        using var launch = new Launch();
        using var host = PluginHost.Start(Manifest(), (_, _) => null);
        Assert.False(host.HasPlatform);
        var error = Assert.Throws<PlatformCallException>(() => host.CallPlatform("GET", "/api/v1/users/me"));
        Assert.Contains("not configured", error.Message);
    }
}
