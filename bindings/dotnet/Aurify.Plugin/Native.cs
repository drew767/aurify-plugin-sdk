using System.Runtime.InteropServices;
using System.Text;

namespace Aurify.Plugin;

/// <summary>
/// The C ABI of the native library, one entry per function in <c>aurify_plugin.h</c>.
/// Strings are handed over as NUL-terminated UTF-8 buffers pinned for the call; strings
/// received are copied out and returned to the library's allocator at once, so no
/// native pointer outlives the call that produced it.
/// </summary>
internal static class Native
{
    private const string Library = "aurify_plugin";

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate IntPtr OperationFn(IntPtr name, IntPtr argsJson, IntPtr userData);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr aurify_plugin_version();

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr aurify_plugin_string_alloc(IntPtr text);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern void aurify_plugin_string_free(IntPtr text);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr aurify_plugin_manifest_validate(IntPtr manifestJson);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr aurify_plugin_host_start(IntPtr manifestJson, IntPtr launchJson, OperationFn onOperation, IntPtr userData, out IntPtr errorOut);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern ushort aurify_plugin_host_port(IntPtr host);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern void aurify_plugin_host_set_health(IntPtr host, IntPtr status, IntPtr message);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    [return: MarshalAs(UnmanagedType.U1)]
    public static extern bool aurify_plugin_host_has_platform(IntPtr host);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr aurify_plugin_platform_call(IntPtr host, IntPtr method, IntPtr path, IntPtr bodyJson, out IntPtr errorOut);

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    public static extern void aurify_plugin_host_stop(IntPtr host);

    /// <summary>A NUL-terminated UTF-8 copy of a managed string, pinned until disposed.</summary>
    public readonly struct Utf8Arg : IDisposable
    {
        private readonly GCHandle _handle;

        public Utf8Arg(string? text)
        {
            if (text is null)
            {
                _handle = default;
                Pointer = IntPtr.Zero;
                return;
            }
            var bytes = new byte[Encoding.UTF8.GetByteCount(text) + 1];
            Encoding.UTF8.GetBytes(text, 0, text.Length, bytes, 0);
            _handle = GCHandle.Alloc(bytes, GCHandleType.Pinned);
            Pointer = _handle.AddrOfPinnedObject();
        }

        public IntPtr Pointer { get; }

        public void Dispose()
        {
            if (_handle.IsAllocated) _handle.Free();
        }
    }

    /// <summary>Reads a library-owned string and frees it. NULL reads as null.</summary>
    public static string? TakeString(IntPtr pointer)
    {
        if (pointer == IntPtr.Zero) return null;
        try
        {
            return Marshal.PtrToStringUTF8(pointer);
        }
        finally
        {
            aurify_plugin_string_free(pointer);
        }
    }

    /// <summary>Reads a static string the library keeps forever. Never freed.</summary>
    public static string ReadStatic(IntPtr pointer) => Marshal.PtrToStringUTF8(pointer) ?? string.Empty;

    /// <summary>
    /// Copies managed text into a string owned by the library, as the operation callback
    /// must return. The library frees it after use.
    /// </summary>
    public static IntPtr AllocForLibrary(string text)
    {
        using var arg = new Utf8Arg(text);
        return aurify_plugin_string_alloc(arg.Pointer);
    }
}
