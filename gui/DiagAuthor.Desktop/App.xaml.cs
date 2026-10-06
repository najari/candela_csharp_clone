using System.Diagnostics;
using System.IO;
using System.Windows;

namespace DiagAuthor.Desktop;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        var smokeDirectory = SmokeTest.ParseDirectory(e.Args);
        var forceMock = e.Args.Any(a => string.Equals(a, "--mock", StringComparison.OrdinalIgnoreCase));
        EngineClient? engine = null;
        var engineError = "";
        if (!forceMock) engine = EngineClient.TryCreate(out engineError);

        try
        {
            var window = new MainWindow(engine, engineError, forceMock);
            MainWindow = window;
            if (smokeDirectory is null)
            {
                window.Show();
                return;
            }

            ShutdownMode = ShutdownMode.OnExplicitShutdown;
            window.Show();
            _ = SmokeTest.RunAsync(window, smokeDirectory);
        }
        catch (Exception ex)
        {
            engine?.Dispose();
            WriteLastError(ex);
            if (smokeDirectory is not null)
            {
                SmokeTest.WriteStartupFailure(smokeDirectory, ex);
                Shutdown(1);
                return;
            }

            MessageBox.Show(
                ex.Message + Environment.NewLine + Environment.NewLine + "로그 / log: " + LastErrorPath(),
                "DiagAuthor 시작 실패 / startup failed",
                MessageBoxButton.OK, MessageBoxImage.Error);
            Shutdown(1);
        }
    }

    private static string LastErrorPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "DiagAuthor", "last-error.txt");

    private static void WriteLastError(Exception ex)
    {
        try
        {
            var path = LastErrorPath();
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, ex.ToString());
        }
        catch (IOException io)
        {
            Debug.WriteLine("Failed to write " + LastErrorPath() + ": " + io.Message);
        }
        catch (UnauthorizedAccessException io)
        {
            Debug.WriteLine("Failed to write " + LastErrorPath() + ": " + io.Message);
        }
    }
}
