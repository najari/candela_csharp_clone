using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Globalization;
using System.Runtime.CompilerServices;
using System.Text.Json.Nodes;

namespace DiagAuthor.Desktop;

/// <summary>Where the shell gets its rows from. Mock mode is always badged in the UI.</summary>
internal enum DataMode
{
    Engine,
    Mock,
}

/// <summary>Typed view of the engine's `hello` result (plan docs/17 4절).</summary>
internal sealed class HelloInfo
{
    public int Protocol { get; init; }

    /// <summary>"&lt;engine name&gt; &lt;semver&gt;" as reported by the engine.</summary>
    public string EngineVersion { get; init; } = "";

    /// <summary>Highest project schema_version this engine can open. A file above
    /// this must be refused with UNSUPPORTED_VERSION rather than opened.</summary>
    public int SchemaVersionMax { get; init; }

    /// <summary>Engine name part of <see cref="EngineVersion"/>, for display.</summary>
    public string Engine => EngineVersion.Split(' ', 2)[0];

    /// <summary>Semantic version part of <see cref="EngineVersion"/>, for display.</summary>
    public string Version => EngineVersion.Split(' ', 2).Length > 1
        ? EngineVersion.Split(' ', 2)[1]
        : EngineVersion;
}

/// <summary>One node of the navigation tree (mirrors `tree.get`).</summary>
public sealed class TreeItem : INotifyPropertyChanged
{
    private bool isExpanded;
    private bool isSelected;

    public string Id { get; init; } = "";
    public string Kind { get; init; } = "";
    public string Label { get; init; } = "";
    public int ChildCount { get; init; }
    public TreeItem? Parent { get; private set; }
    public ObservableCollection<TreeItem> Children { get; } = new();

    public bool IsExpanded
    {
        get => isExpanded;
        set
        {
            if (isExpanded == value) return;
            isExpanded = value;
            OnPropertyChanged();
        }
    }

    public bool IsSelected
    {
        get => isSelected;
        set
        {
            if (isSelected == value) return;
            isSelected = value;
            OnPropertyChanged();
        }
    }

    /// <summary>Small text glyph instead of an image asset (no image files in this project).</summary>
    public string Glyph => Kind switch
    {
        "ecu" => "\u25A3",
        "variant" => "\u25C6",
        "class" => "\u25C7",
        "instance" => "\u25CB",
        "did" => "\u25AA",
        "dtc" => "\u25B4",
        _ => "\u00B7",
    };

    public string CountText => ChildCount > 0 ? $"({ChildCount.ToString(CultureInfo.InvariantCulture)})" : "";

    public string PathText
    {
        get
        {
            var parts = new List<string>();
            for (var node = this; node is not null; node = node.Parent) parts.Add(node.Label);
            parts.Reverse();
            return "/" + string.Join("/", parts);
        }
    }

    public void AddChild(TreeItem child)
    {
        child.Parent = this;
        Children.Add(child);
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    /// <summary>Converts the `{"nodes":[...]}` payload into view models.</summary>
    public static List<TreeItem> FromJson(JsonNode? nodes)
    {
        var list = new List<TreeItem>();
        if (nodes is not JsonArray array) return list;
        foreach (var node in array)
        {
            if (node is null) continue;
            var item = new TreeItem
            {
                Id = node["id"]?.ToString() ?? "",
                Kind = node["kind"]?.ToString() ?? "",
                Label = node["label"]?.ToString() ?? "",
                ChildCount = node["child_count"]?.GetValue<int>() ?? 0,
            };
            foreach (var child in FromJson(node["children"])) item.AddChild(child);
            list.Add(item);
        }
        return list;
    }
}

/// <summary>
/// One row of the object list. Rows are replaced wholesale on every load, so the
/// row type itself is immutable; only the display index is assigned after filtering.
/// </summary>
internal sealed class ObjectRow
{
    public required string Id { get; init; }
    public required string Kind { get; init; }
    public string Number { get; init; } = "";
    public string Name { get; init; } = "";
    public string Type { get; init; } = "";
    public string Data { get; init; } = "";
    public string Shared { get; init; } = "";
    public string Qualifier { get; init; } = "";
    public string Status { get; init; } = "";
    public string RowNo { get; set; } = "";
    public string SearchText { get; init; } = "";

    public static string BuildSearchText(params string[] fields) =>
        string.Join(" ", fields.Where(f => f.Length > 0)).ToLowerInvariant();

    /// <summary>
    /// Converts an `objects.query` result. The engine emits `{id, kind, name}` plus
    /// kind-specific extras (`identifier` for did, `code_hex` for dtc, `qualifier`
    /// for ecu/variant/class/instance); missing keys simply render blank.
    /// </summary>
    public static List<ObjectRow> FromQuery(JsonNode? result, string kind)
    {
        var rows = new List<ObjectRow>();
        if (result?["rows"] is not JsonArray array) return rows;
        foreach (var node in array)
        {
            if (node is null) continue;
            var number = node["identifier"]?.ToString() ?? node["code_hex"]?.ToString() ?? node["service_id_hex"]?.ToString() ?? "";
            var name = node["name"]?.ToString() ?? "";
            var qualifier = node["qualifier"]?.ToString() ?? "";
            rows.Add(new ObjectRow
            {
                Id = node["id"]?.ToString() ?? "",
                Kind = kind,
                Number = number,
                Name = name,
                Qualifier = qualifier,
                SearchText = BuildSearchText(number, name, qualifier, kind),
            });
        }
        return rows;
    }
}

/// <summary>One row of the output dock's work log.</summary>
internal sealed class LogEntry
{
    public int No { get; init; }
    public required string Kind { get; init; }
    public required string Time { get; init; }
    public required string Origin { get; init; }
    public required string Message { get; init; }
    public string Detail { get; init; } = "";
    public string LinkText { get; init; } = "";
    public string? LinkKind { get; init; }
    public string? LinkId { get; init; }
}

/// <summary>One row of a `validation.run` result.</summary>
internal sealed class ValidationIssue
{
    public string Rule { get; init; } = "";
    public string Severity { get; init; } = "info";
    public string Object { get; init; } = "";
    public string Field { get; init; } = "";
    public string Message { get; init; } = "";

    public string SeverityText => Severity switch
    {
        "error" => "오류",
        "warning" => "경고",
        _ => "정보",
    };

    public static List<ValidationIssue> FromJson(JsonNode? result)
    {
        var list = new List<ValidationIssue>();
        if (result?["issues"] is not JsonArray array) return list;
        foreach (var node in array)
        {
            if (node is null) continue;
            list.Add(new ValidationIssue
            {
                Rule = node["rule"]?.ToString() ?? "",
                Severity = node["severity"]?.ToString() ?? "info",
                Object = node["object"]?.ToString() ?? "",
                Field = node["field"]?.ToString() ?? "",
                Message = node["message"]?.ToString() ?? "",
            });
        }
        return list;
    }
}

/// <summary>Label/value pair of the selected-object property sheet.</summary>
internal sealed record PropertyRow(string Label, string Value, bool Mono = false);

/// <summary>Row of the column-information overlay.</summary>
internal sealed record ColumnInfoRow(string Header, string Qualifier, string Description);
