using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;

namespace DiagAuthor.Desktop;

// Wire contract with diagauthor-engine (JSON Lines, one request/response per line):
//   request  = {"id":<int>,"op":"<snake_case>","args":{...}}
//   response = {"id":<int>,"ok":true,"result":{...}} | {"id":<int>,"ok":false,"error":"..."}
// Keys are snake_case end to end.
public sealed class EngineRequest
{
    [JsonPropertyName("id")] public long Id { get; init; }
    [JsonPropertyName("op")] public required string Op { get; init; }
    [JsonPropertyName("args")] public JsonNode? Args { get; init; }
}

public sealed class EngineResponse
{
    [JsonPropertyName("id")] public long Id { get; init; }
    [JsonPropertyName("ok")] public bool Ok { get; init; }
    [JsonPropertyName("result")] public JsonNode? Result { get; init; }
    [JsonPropertyName("error")] public string? Error { get; init; }
}

public sealed class EngineHello
{
    public int Protocol { get; init; }
    public string Engine { get; init; } = "";
    public string Version { get; init; } = "";
    public int SchemaVersion { get; init; }
}

public static class Protocol
{
    public static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        PropertyNameCaseInsensitive = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public const int Version = 1;
}
