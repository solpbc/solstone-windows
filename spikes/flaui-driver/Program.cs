using System;
using System.IO;
using System.Threading;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;

var logPath = @"C:\Temp\observer-spike\flaui.txt";
Directory.CreateDirectory(Path.GetDirectoryName(logPath));
File.WriteAllText(logPath, $"started={DateTime.Now:o} cwd={Environment.CurrentDirectory}{Environment.NewLine}");
void Log(string line) => File.AppendAllText(logPath, line + Environment.NewLine);

try
{
    var appPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "SolObserverSpike",
        "WinFormsTarget",
        "UiTarget.exe");
    Log($"app={appPath}");

    using (var app = Application.Launch(appPath))
    using (var automation = new UIA3Automation())
    {
        var window = app.GetMainWindow(automation, TimeSpan.FromSeconds(10));
        if (window == null)
        {
            Log("FLAUI_FAIL: no main window");
            Environment.Exit(2);
        }
        Log($"window={window.Title}");

        var button = window.FindFirstDescendant(cf => cf.ByAutomationId("PrimaryButton"))?.AsButton();
        var status = window.FindFirstDescendant(cf => cf.ByAutomationId("StatusBox"))?.AsTextBox();
        if (button == null || status == null)
        {
            Log("FLAUI_FAIL: controls not found");
            Environment.Exit(3);
        }

        Log($"initial={status.Text}");
        button.Invoke();
        string observed = status.Text;
        for (var i = 0; i < 20; i++)
        {
            observed = status.Text;
            if (observed == "Clicked") break;
            Thread.Sleep(100);
        }

        Log($"status={observed}");
        if (observed == "Clicked")
        {
            Log("FLAUI_OK");
            app.Close();
            Environment.Exit(0);
        }

        Log("FLAUI_FAIL: click did not update status");
        app.Close();
        Environment.Exit(4);
    }
}
catch (Exception ex)
{
    Log("FLAUI_EXCEPTION");
    Log(ex.ToString());
    Environment.Exit(10);
}
