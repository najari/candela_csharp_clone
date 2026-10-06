using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;

namespace DiagAuthor.Desktop;

/// <summary>
/// `--smoke-test &lt;dir&gt;` harness: drives the real window, runs a `hello`
/// round-trip, loads 20,000 rows, exercises tree/list/tabs/dock, writes
/// smoke-report.json and exits non-zero on any failed assertion.
/// </summary>
internal static class SmokeTest
{
    private static readonly JsonSerializerOptions ReportJson = new() { WriteIndented = true };

    internal static string? ParseDirectory(string[] args)
    {
        for (var i = 0; i < args.Length; i++)
        {
            if (!string.Equals(args[i], "--smoke-test", StringComparison.OrdinalIgnoreCase)) continue;
            var next = i + 1 < args.Length ? args[i + 1] : null;
            return Path.GetFullPath(next is null || next.StartsWith("--", StringComparison.Ordinal) ? Path.Combine("artifacts", "smoke") : next);
        }
        return null;
    }

    internal static void WriteStartupFailure(string output, Exception ex)
    {
        try
        {
            Directory.CreateDirectory(output);
            var failure = new
            {
                passed = false,
                stage = "startup",
                error = ex.Message,
                detail = ex.ToString(),
                at = DateTimeOffset.Now,
            };
            File.WriteAllText(Path.Combine(output, "smoke-report.json"), JsonSerializer.Serialize(failure, ReportJson));
        }
        catch (IOException io)
        {
            Debug.WriteLine("Failed to write startup failure report: " + io.Message);
        }
    }

    internal static async Task RunAsync(MainWindow window, string output)
    {
        Directory.CreateDirectory(output);
        var steps = new List<object>();
        var stopwatch = Stopwatch.StartNew();
        var screenshots = new List<string>();
        var deliveredPath = "";
        var deliveredHash = "";

        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException("Smoke assertion failed: " + name);
            steps.Add(new { name, passed = true });
        }

        try
        {
            await window.ReadyTask.WaitAsync(TimeSpan.FromSeconds(30));
            await WaitIdleAsync(window);
            Check(window.IsVisible, "window is visible");
            Check(FindVisual<DataGrid>(window) is not null, "native DataGrid present in visual tree");
            Check(FindVisual<TreeView>(window) is not null, "native TreeView present in visual tree");
            var webviewAssemblies = AppDomain.CurrentDomain.GetAssemblies()
                .Count(a => (a.GetName().Name ?? "").Contains("WebView2", StringComparison.OrdinalIgnoreCase));
            Check(webviewAssemblies == 0, "no WebView2 assembly loaded");

            Check(window.Hello.Protocol == Protocol.Version, $"hello protocol == {Protocol.Version}");
            Check(!string.IsNullOrEmpty(window.Hello.Engine), "hello engine name is non-empty");
            Check(!string.IsNullOrEmpty(window.Hello.Version), "hello engine version is non-empty");
            Check(window.Hello.SchemaVersionMax >= 1, $"hello schema_version_max >= 1 (got {window.Hello.SchemaVersionMax})");

            var projectPath = "";
            if (window.Mode == DataMode.Engine)
            {
                var raw = await window.Engine!.CallAsync("hello");
                Check(raw["protocol"]?.GetValue<int>() == 1
                      && raw["engine_version"]?.ToString()?.StartsWith("diagauthor-engine ", StringComparison.Ordinal) == true
                      && raw["schema_version_max"]?.GetValue<int>() >= 1,
                    "engine hello round-trip (protocol=1, engine_version=diagauthor-engine <ver>, schema_version_max>=1)");
                projectPath = Path.Combine(output, "smoke-" + Guid.NewGuid().ToString("N") + ".diagproj");
                await window.CreateProjectForTestAsync(projectPath);
                Check(window.TreeRootCount >= 1, "tree.get returns the created ECU node");
                var status = await window.Engine.CallAsync("project.status");
                Check(status["open"]?.GetValue<bool>() == true, "project.status open == true");
                Check(string.Equals(status["ecu"]?.ToString(), "Smoke ECU", StringComparison.Ordinal), "project.status ecu == Smoke ECU");
            }
            else
            {
                Check(window.TreeRootCount >= 1, "mock tree has a root ECU node");
                Check(window.VisibleRowCount > 0, "mock list shows rows");
            }

            var shellShot = Path.Combine(output, "wpf-shell.png");
            await SnapshotAsync(window, shellShot);
            screenshots.Add(shellShot);

            var loadMs = window.LoadPerformanceRowsForTest(20000);
            Check(window.RowCount == 20000, "20,000 rows loaded into the grid source");
            var realized = window.RealizedRowCountForTest();
            Check(realized < 1000, $"row virtualization realized only {realized} containers");
            var responsiveMs = await window.MeasureDispatcherAsync();
            Check(responsiveMs < 2000, $"UI dispatcher stayed responsive ({responsiveMs} ms)");
            var scrollMs = await window.ScrollToEndAsync();
            Check(window.VisibleRowCount == 20000, "all rows remain in the virtualized view");

            var gridShot = Path.Combine(output, "wpf-20k-rows.png");
            await SnapshotAsync(window, gridShot);
            screenshots.Add(gridShot);

            var filterMs = window.ApplyFilterForTest("perf-0001", false);
            Check(window.VisibleRowCount > 0 && window.VisibleRowCount < 20000,
                $"word filter narrowed 20,000 rows to {window.VisibleRowCount}");
            var badRegexRows = window.VisibleRowCount;
            window.ApplyFilterForTest("[", true);
            Check(window.VisibleRowCount == badRegexRows, "invalid regex left the list untouched");
            window.ApplyFilterForTest("", false);
            Check(window.VisibleRowCount == 20000, "clearing the filter restored all rows");

            for (var tab = 0; tab < 4; tab++)
            {
                window.SelectTabForTest(tab);
                Check(window.CurrentTab == tab, $"tab {tab} selected");
            }

            await window.SelectKindForTestAsync("did");
            Check(window.CurrentKind == "did", "tree DID node switches the list to DID rows");
            await window.SelectKindForTestAsync("dtc");
            Check(window.CurrentKind == "dtc", "tree DTC node switches the list to DTC rows");

            await window.RunValidationForTestAsync();
            Check(window.IssueCount >= 0, $"validation.run returned {window.IssueCount} issues");
            if (window.Mode == DataMode.Mock) Check(window.IssueCount > 0, "mock validation has labelled issues");
            else Check(window.IssueCount == 0, "engine M1 validation rule set is empty");

            window.SetDockCollapsedForTest(true);
            Check(window.IsDockCollapsed, "output dock collapsed");
            window.SetDockCollapsedForTest(false);
            Check(!window.IsDockCollapsed, "output dock expanded");

            // M1 save/snapshot over the real engine. The delivery path must
            // differ from the working copy: D-9 keeps the WAL working file and
            // the sidecar-free delivery snapshot separate, and Windows locks the
            // open working file.
            if (window.Mode == DataMode.Engine)
            {
                // project.snapshot errors when no project is open, so a
                // successful call already proves the session has one.
                var beforeSave = await window.Engine!.CallAsync("project.snapshot");
                Check(string.Equals(beforeSave["path"]?.ToString(), projectPath, StringComparison.Ordinal),
                    "project.snapshot reports the working copy path");
                Check(beforeSave["schema_version_max"]?.GetValue<int>() >= 1,
                    "project.snapshot reports schema_version_max");
                Check(beforeSave["last_saved"] is null
                      || beforeSave["last_saved"]!.GetValueKind() == JsonValueKind.Null,
                    "project.snapshot has no delivery recorded before the first save");

                var delivery = Path.Combine(output, "smoke-delivered-" + Guid.NewGuid().ToString("N") + ".diagproj");
                var receipt = await window.Engine.CallAsync("project.save", new JsonObject
                {
                    ["path"] = delivery,
                });
                Check(receipt["path"]?.ToString() == delivery, "project.save wrote the requested delivery path");
                Check(File.Exists(delivery), "project.save produced the delivery file");
                Check(!File.Exists(delivery + "-wal") && !File.Exists(delivery + "-shm"),
                    "delivery file has no -wal/-shm sidecar");
                var hash = receipt["file_hash"]?.ToString() ?? "";
                Check(hash.Length == 32, $"project.save receipt carries a content hash ({hash.Length} hex chars)");
                Check(receipt["dirty"]?.GetValue<bool>() == false, "project.save clears the dirty flag");

                // The delivery must be a project on its own.
                var stillOpen = await window.Engine.CallAsync("project.status");
                Check(stillOpen["open"]?.GetValue<bool>() == true,
                    "project.save leaves the working copy open for continued editing");

                var afterSave = await window.Engine.CallAsync("project.snapshot");
                Check(afterSave["last_saved"]?["file_hash"]?.ToString() == hash,
                    "project.snapshot reports the last delivery hash");
                deliveredPath = delivery;
                deliveredHash = hash;

                // Writing over the open working copy must be refused, not attempted.
                var inPlace = await window.Engine.CallAsync("project.save", new JsonObject
                {
                    ["path"] = projectPath,
                }, CancellationToken.None, expectError: true);
                Check(inPlace.ToString().Contains("working copy"),
                    "project.save refuses to overwrite the open working copy");
            }

            var finalShot = Path.Combine(output, "wpf-final.png");
            await SnapshotAsync(window, finalShot);
            screenshots.Add(finalShot);

            var report = new
            {
                passed = true,
                mode = window.Mode.ToString().ToLowerInvariant(),
                hello = new
                {
                    protocol = window.Hello.Protocol,
                    engine_version = window.Hello.EngineVersion,
                    schema_version_max = window.Hello.SchemaVersionMax,
                },
                project = projectPath,
                tree_root_nodes = window.TreeRootCount,
                rows_loaded = 20000,
                load_ms = loadMs,
                realized_rows = realized,
                responsive_ms = responsiveMs,
                scroll_ms = scrollMs,
                filter_ms = filterMs,
                filter_visible_rows = badRegexRows,
                validation_issues = window.IssueCount,
                webview2_assemblies = webviewAssemblies,
                status_line = window.StatusLine,
                // Save evidence, empty in mock mode where no engine is involved.
                delivery = deliveredPath,
                delivery_hash = deliveredHash,
                screenshots,
                elapsed_ms = stopwatch.ElapsedMilliseconds,
                steps,
            };
            File.WriteAllText(Path.Combine(output, "smoke-report.json"), JsonSerializer.Serialize(report, ReportJson));
            window.Close();
            Application.Current.Shutdown(0);
        }
        catch (Exception ex)
        {
            var failure = new
            {
                passed = false,
                mode = window.Mode.ToString().ToLowerInvariant(),
                error = ex.Message,
                detail = ex.ToString(),
                elapsed_ms = stopwatch.ElapsedMilliseconds,
                screenshots,
                steps,
            };
            File.WriteAllText(Path.Combine(output, "smoke-report.json"), JsonSerializer.Serialize(failure, ReportJson));
            window.Close();
            Application.Current.Shutdown(1);
        }
    }

    private static async Task WaitIdleAsync(MainWindow window)
    {
        await window.Dispatcher.InvokeAsync(() => { }, DispatcherPriority.ApplicationIdle);
        window.UpdateLayout();
    }

    private static T? FindVisual<T>(DependencyObject root) where T : DependencyObject
    {
        var children = VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < children; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T match) return match;
            var nested = FindVisual<T>(child);
            if (nested is not null) return nested;
        }
        return null;
    }

    private static async Task SnapshotAsync(MainWindow window, string file)
    {
        await Task.Delay(150);
        await window.Dispatcher.InvokeAsync(() => { }, DispatcherPriority.ApplicationIdle);
        window.UpdateLayout();
        var visual = (FrameworkElement)window.Content;
        if (visual.ActualWidth < 10 || visual.ActualHeight < 10) throw new InvalidOperationException("Window has no renderable size for the screenshot.");
        var bitmap = new RenderTargetBitmap(
            (int)Math.Ceiling(visual.ActualWidth),
            (int)Math.Ceiling(visual.ActualHeight),
            96, 96, PixelFormats.Pbgra32);
        bitmap.Render(visual);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var stream = File.Create(file);
        encoder.Save(stream);
    }
}
