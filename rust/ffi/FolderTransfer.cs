// folder-transfer .NET binding (P/Invoke over ft.dll / libft.so).
// Put ft.dll next to your executable (or on PATH). Build target: any .NET that
// supports DllImport (Framework or .NET Core/5+).
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;

public static class FolderTransfer
{
    const string LIB = "ft"; // resolves to ft.dll on Windows, libft.so on Linux

    [DllImport(LIB, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    static extern int ft_get(string server, ushort port, string token, string fingerprint,
                             string toFolder, string ignore, int streams);

    [DllImport(LIB, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    static extern IntPtr ft_serve_start(string folder, ushort port, int streams, string ignore,
                                        int noCompress, int once,
                                        byte[] outToken, UIntPtr outTokenLen,
                                        byte[] outFingerprint, UIntPtr outFingerprintLen);

    [DllImport(LIB, CallingConvention = CallingConvention.Cdecl)]
    static extern int ft_serve_wait(IntPtr handle);

    [DllImport(LIB, CallingConvention = CallingConvention.Cdecl)]
    static extern int ft_last_error(byte[] buf, UIntPtr len);

    public static string LastError()
    {
        var buf = new byte[1024];
        ft_last_error(buf, (UIntPtr)buf.Length);
        int n = Array.IndexOf(buf, (byte)0); if (n < 0) n = buf.Length;
        return Encoding.UTF8.GetString(buf, 0, n);
    }

    /// <summary>Pull a folder from a server into <paramref name="toFolder"/>. Throws on failure.</summary>
    public static void Get(string server, ushort port, string token, string fingerprint,
                           string toFolder, string ignore = null, int streams = 0)
    {
        if (ft_get(server, port, token, fingerprint, toFolder, ignore, streams) != 0)
            throw new Exception("ft_get failed: " + LastError());
    }

    public sealed class Server
    {
        IntPtr _h;
        public string Token { get; }
        public string Fingerprint { get; }

        /// <summary>Start serving <paramref name="folder"/> in the background; Token/Fingerprint
        /// are ready immediately to hand to the receiver. Dispose/Wait() to block until done.</summary>
        public Server(string folder, ushort port, int streams = 4, string ignore = null,
                      bool compress = true, bool once = true)
        {
            var tok = new byte[64];
            var fp = new byte[128];
            _h = ft_serve_start(folder, port, streams, ignore, compress ? 0 : 1, once ? 1 : 0,
                                tok, (UIntPtr)tok.Length, fp, (UIntPtr)fp.Length);
            if (_h == IntPtr.Zero) throw new Exception("ft_serve_start failed: " + LastError());
            Token = CStr(tok); Fingerprint = CStr(fp);
        }

        /// <summary>Block until the server finishes (e.g. after one client with once=true).</summary>
        public int Wait() { var h = _h; _h = IntPtr.Zero; return h == IntPtr.Zero ? 0 : ft_serve_wait(h); }

        static string CStr(byte[] b) { int n = Array.IndexOf(b, (byte)0); if (n < 0) n = b.Length; return Encoding.UTF8.GetString(b, 0, n); }
    }

    // Example: move D:\data from this machine to a remote one that runs Get(...).
    public static async Task Example()
    {
        var srv = new Server(@"D:\data", 8722, streams: 4, once: true);
        Console.WriteLine($"token={srv.Token} fingerprint={srv.Fingerprint}");
        // ... transmit srv.Token + srv.Fingerprint (+ this host's IP) to the receiver,
        //     which calls FolderTransfer.Get(thisHostIp, 8722, token, fingerprint, @"E:\incoming");
        await Task.Run(() => srv.Wait()); // don't block the UI thread
    }
}
