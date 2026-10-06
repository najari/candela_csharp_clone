using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace DiagAuthor.Desktop;

public sealed class EngineClient : IDisposable
{
    private const int ErrorBufferLines = 200;
    private static readonly TimeSpan ResponseTimeout = TimeSpan.FromSeconds(60);

    private readonly Process process;
    private readonly SemaphoreSlim gate = new(1, 1);
    private readonly Queue<string> errors = new();
    private readonly object errorLock = new();
    private long sequence;
    private bool disposed;

    public event Action<string>? Exited;

    /// <summary>
    /// Locates the engine binary, preferring a copy deployed next to the
    /// application and falling back to the cargo build output in the repository
    /// (target\release, then target\debug). Returns null with a reason when no
    /// candidate exists, so the shell can fall back to clearly-badged mock mode
    /// instead of crashing.
    /// </summary>
    public static string? FindEngineExecutable(string? explicitPath = null)
    {
        if (!string.IsNullOrWhiteSpace(explicitPath))
            return File.Exists(explicitPath) ? explicitPath : null;

        var beside = Path.Combine(AppContext.BaseDirectory, "diagauthor-engine.exe");
        if (File.Exists(beside)) return beside;

        // Walk up from bin\<config>\net10.0-windows\win-x64\ to the repo root.
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        for (var depth = 0; depth < 8 && directory is not null; depth++, directory = directory.Parent)
        {
            var target = Path.Combine(directory.FullName, "target");
            if (!Directory.Exists(target)) continue;
            foreach (var configuration in new[] { "release", "debug" })
            {
                var candidate = Path.Combine(target, configuration, "diagauthor-engine.exe");
                if (File.Exists(candidate)) return candidate;
            }
        }
        return null;
    }

    /// <summary>
    /// Creates a client when the engine binary can be located; returns null (with
    /// a reason for the UI banner) when it cannot, so the shell can fall back to
    /// clearly-badged mock mode instead of crashing.
    /// </summary>
    public static EngineClient? TryCreate(out string error, string? executable = null)
    {
        try
        {
            error = "";
            return new EngineClient(executable);
        }
        catch (Exception ex) when (ex is FileNotFoundException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            error = ex.Message;
            return null;
        }
    }

    public EngineClient(string? executable = null)
    {
        executable ??= FindEngineExecutable()
            ?? Path.Combine(AppContext.BaseDirectory, "diagauthor-engine.exe");
        if (!File.Exists(executable))
        {
            throw new FileNotFoundException(
                "진단 엔진을 찾을 수 없습니다: " + executable +
                " · The Rust engine (diagauthor-engine.exe) was not found next to DiagAuthor.exe " +
                "nor under the repository target\\release or target\\debug. " +
                "Build it with scripts\\build.cmd, or copy diagauthor-engine.exe into the application folder.",
                executable);
        }

        process = new Process
        {
            StartInfo = new ProcessStartInfo(executable, "serve")
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                StandardInputEncoding = new UTF8Encoding(false),
                StandardOutputEncoding = new UTF8Encoding(false),
                StandardErrorEncoding = new UTF8Encoding(false),
                WorkingDirectory = AppContext.BaseDirectory,
            },
            EnableRaisingEvents = true,
        };
        process.Exited += (_, _) => Exited?.Invoke(
            $"엔진 프로세스가 종료되었습니다 (engine exited with code {process.ExitCode})." + Environment.NewLine + RecentErrors());
        process.Start();
        _ = ReadErrorsAsync(process.StandardError);
    }

    public bool HasExited => process.HasExited;

    public string RecentErrors()
    {
        lock (errorLock) return errors.Count == 0 ? "(stderr 없음)" : string.Join(Environment.NewLine, errors);
    }

    /// <summary>
    /// Sends one request. With <paramref name="expectError"/> the engine's error
    /// string is returned instead of being thrown, which lets a caller assert a
    /// refusal (an in-place save, an unsupported version) without catching.
    /// </summary>
    public async Task<JsonNode> CallAsync(
        string op,
        JsonNode? args = null,
        CancellationToken cancellationToken = default,
        bool expectError = false)
    {
        await gate.WaitAsync(cancellationToken);
        try
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (process.HasExited)
            {
                throw new IOException("진단 엔진이 종료되었습니다 (engine process is not running). " + RecentErrors());
            }

            var id = ++sequence;
            var request = new EngineRequest { Id = id, Op = op, Args = args ?? new JsonObject() };
            await process.StandardInput.WriteLineAsync(JsonSerializer.Serialize(request, Protocol.JsonOptions));
            await process.StandardInput.FlushAsync();

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(ResponseTimeout);
            string? line;
            try
            {
                line = await process.StandardOutput.ReadLineAsync(timeout.Token);
            }
            catch (OperationCanceledException)
            {
                process.Kill(true);
                throw new IOException($"엔진 응답 시간이 초과되었습니다 (no response within {ResponseTimeout.TotalSeconds:0}s, engine killed). " + RecentErrors());
            }

            if (line is null)
            {
                throw new IOException("엔진 연결이 끊어졌습니다 (engine stdout closed). " + RecentErrors());
            }

            EngineResponse? response;
            try
            {
                response = JsonSerializer.Deserialize<EngineResponse>(line, Protocol.JsonOptions);
            }
            catch (JsonException ex)
            {
                throw new IOException("엔진 응답을 해석할 수 없습니다 (invalid JSON line): " + line, ex);
            }

            if (response is null) throw new IOException("엔진 응답이 비어 있습니다 (empty response).");
            if (response.Id != id) throw new IOException($"엔진 응답 ID 불일치 (response id {response.Id}, expected {id}).");
            if (!response.Ok)
            {
                var message = response.Error ?? "Engine error";
                if (expectError) return JsonValue.Create(message)!;
                throw new InvalidOperationException(message);
            }
            return response.Result?.DeepClone() ?? new JsonObject();
        }
        finally
        {
            gate.Release();
        }
    }

    private async Task ReadErrorsAsync(StreamReader reader)
    {
        while (await reader.ReadLineAsync() is { } line)
        {
            lock (errorLock)
            {
                errors.Enqueue(line);
                while (errors.Count > ErrorBufferLines) errors.Dequeue();
            }
        }
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        try
        {
            process.StandardInput.Close();
            if (!process.WaitForExit(1500)) process.Kill(true);
        }
        catch (InvalidOperationException ex)
        {
            Debug.WriteLine("EngineClient.Dispose: process already exited (" + ex.Message + ")");
        }
        catch (System.ComponentModel.Win32Exception ex)
        {
            Debug.WriteLine("EngineClient.Dispose: kill skipped (" + ex.Message + ")");
        }
        process.Dispose();
    }
}
