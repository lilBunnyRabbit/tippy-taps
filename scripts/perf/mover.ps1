param([int]$Seconds = 60, [int]$Hz = 1000)
Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
public static class Mover {
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint data, flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public MOUSEINPUT mi; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint p);
    public static long Run(int seconds, int hz) {
        timeBeginPeriod(1);
        var input = new INPUT[1];
        input[0].type = 0; input[0].mi.flags = 1;
        int[] dx = { 2, 2, 0, -2, -2, -2, 0, 2 }; int[] dy = { 0, 2, 2, 2, 0, -2, -2, -2 };
        var sw = Stopwatch.StartNew(); long sent = 0;
        while (sw.ElapsedMilliseconds < seconds * 1000L) {
            long due = sent * Stopwatch.Frequency / hz;
            while (sw.ElapsedTicks < due) { System.Threading.Thread.SpinWait(50); }
            int i = (int)(sent / 25 % 8);
            input[0].mi.dx = dx[i]; input[0].mi.dy = dy[i];
            if (SendInput(1, input, Marshal.SizeOf(typeof(INPUT))) == 0) { throw new Exception("SendInput failed"); }
            sent++;
        }
        return sent;
    }
}
'@
[Mover]::Run($Seconds, $Hz)
