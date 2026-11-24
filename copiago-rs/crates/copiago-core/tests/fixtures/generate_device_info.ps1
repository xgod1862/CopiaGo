$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class DeviceInfoFixture {
    [StructLayout(LayoutKind.Sequential)]
    private struct Blob {
        public int Length;
        public IntPtr Data;
    }

    [DllImport("crypt32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CryptProtectData(
        ref Blob input, string description, IntPtr entropy, IntPtr reserved,
        IntPtr prompt, uint flags, out Blob output);

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr buffer);

    public static byte[] Protect(byte[] plain) {
        GCHandle pinned = GCHandle.Alloc(plain, GCHandleType.Pinned);
        try {
            var input = new Blob { Length = plain.Length, Data = pinned.AddrOfPinnedObject() };
            Blob output;
            // Same description and CRYPTPROTECT_UI_FORBIDDEN flag as Python pair_store.py.
            if (!CryptProtectData(ref input, "iPhone Clipboard pairing record",
                    IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 1, out output)) {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            try {
                var data = new byte[output.Length];
                Marshal.Copy(output.Data, data, 0, data.Length);
                return data;
            } finally {
                LocalFree(output.Data);
            }
        } finally {
            pinned.Free();
        }
    }
}
'@

$name = [string][char]0x6D4B + [char]0x8BD5 + ' iPhone ' + [char]::ConvertFromUtf32(0x1F4F1)
$json = ConvertTo-Json -Compress -InputObject ([ordered]@{
    name = $name
    model = 'iPhone 15 Pro'
    os_version = '18.1'
})
$magic = [Text.Encoding]::ASCII.GetBytes('ICB-DPAPI1') + [byte]0
$sealed = [DeviceInfoFixture]::Protect([Text.Encoding]::UTF8.GetBytes($json))
[IO.File]::WriteAllBytes($env:COPIAGO_TEST_FIXTURE_PATH, $magic + $sealed)
