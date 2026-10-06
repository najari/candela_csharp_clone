using System.Globalization;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Data;
using System.Windows.Media;

namespace DiagAuthor.Desktop;

/// <summary>One list column: header, width, binding path and a rendering hint.</summary>
internal sealed record ColumnSpec(string Header, double Width, string Binding, bool Numeric = false, string Kind = "text");

/// <summary>
/// Column definitions and small formatting helpers for the shell's lists.
/// Column order follows the approved mockup: row number, identity, name,
/// then the record fields, and Qualifier last (mockup `.grid` / `cols(node)`).
/// </summary>
internal static class Views
{
    public static List<ColumnSpec> ColumnsFor(string kind) => kind == "did"
        ? new List<ColumnSpec>
        {
            new("", 52, nameof(ObjectRow.RowNo), true, "rowno"),
            new("DID", 80, nameof(ObjectRow.Number), true, "mono"),
            new("이름", 210, nameof(ObjectRow.Name)),
            new("진단 클래스", 130, nameof(ObjectRow.Type)),
            new("DID 데이터", 250, nameof(ObjectRow.Data)),
            new("공유 변형", 76, nameof(ObjectRow.Shared)),
            new("Qualifier", 150, nameof(ObjectRow.Qualifier), false, "dim"),
            new("상태", 70, nameof(ObjectRow.Status), false, "status"),
        }
        : new List<ColumnSpec>
        {
            new("", 52, nameof(ObjectRow.RowNo), true, "rowno"),
            new("DTC", 96, nameof(ObjectRow.Number), true, "mono"),
            new("이름", 270, nameof(ObjectRow.Name)),
            new("심각도", 76, nameof(ObjectRow.Type)),
            new("설명", 250, nameof(ObjectRow.Data)),
            new("공유 변형", 76, nameof(ObjectRow.Shared)),
            new("Qualifier", 130, nameof(ObjectRow.Qualifier), false, "dim"),
            new("상태", 70, nameof(ObjectRow.Status), false, "status"),
        };

    public static string LabelFor(string kind) => kind == "did" ? "DID" : "DTC";

    public static string Hex(long value, int digits) =>
        "0x" + value.ToString("X" + digits.ToString(CultureInfo.InvariantCulture), CultureInfo.InvariantCulture);

    /// <summary>SAE J2012 display form of a 24-bit DTC code (mirrors mockup `sae()`).</summary>
    public static string SaeFromCode(long code)
    {
        var letter = "PCBU"[(int)((code >> 22) & 3)];
        return letter
            + ((code >> 16) & 0x3F).ToString("X2", CultureInfo.InvariantCulture)
            + (code & 0xFFFF).ToString("X4", CultureInfo.InvariantCulture);
    }

    public static bool TryParseHex(string text, out long value)
    {
        value = 0;
        var trimmed = text.Trim();
        if (trimmed.StartsWith("0x", StringComparison.OrdinalIgnoreCase)) trimmed = trimmed[2..];
        return trimmed.Length > 0 && long.TryParse(trimmed, NumberStyles.HexNumber, CultureInfo.InvariantCulture, out value);
    }

    public static List<ColumnInfoRow> ColumnInfo(string kind) => kind == "did"
        ? new List<ColumnInfoRow>
        {
            new("DID", "identifier", "DID 번호(16진)"),
            new("이름", "(기본 속성)", "DID 이름"),
            new("진단 클래스", "(파생)", "사용 사례의 진단 클래스"),
            new("DID 데이터", "(파생)", "데이터 구조 요약"),
            new("공유 변형", "(파생)", "이 DID를 쓰는 변형 수"),
            new("Qualifier", "qualifier", "식별 이름. 어느 표든 맨 뒤 열입니다."),
            new("상태", "(파생)", "검증 상태 요약"),
        }
        : new List<ColumnInfoRow>
        {
            new("DTC", "code_hex", "DTC 코드(24비트 · 16진)"),
            new("이름", "(기본 속성)", "오류 텍스트"),
            new("심각도", "(파생)", "DTC 심각도 분류"),
            new("설명", "(파생)", "오류 설명"),
            new("공유 변형", "(파생)", "이 DTC를 쓰는 변형 수"),
            new("Qualifier", "qualifier", "식별 이름. 어느 표든 맨 뒤 열입니다."),
            new("상태", "(파생)", "작성·검증 상태"),
        };
}

/// <summary>
/// List filter with the mockup's [LIST-R500] rules: case-insensitive, words
/// separated by whitespace are OR-ed, "quoted phrases" stay whole. Regex mode
/// replaces the rules with a .NET regular expression (1 s match timeout).
/// </summary>
internal static class RowFilter
{
    private static readonly TimeSpan RegexTimeout = TimeSpan.FromSeconds(1);

    public static bool Matches(ObjectRow row, string filter, bool regex)
    {
        if (string.IsNullOrWhiteSpace(filter)) return true;
        if (regex) return Regex.IsMatch(row.SearchText, filter.Trim(), RegexOptions.IgnoreCase | RegexOptions.CultureInvariant, RegexTimeout);
        foreach (var term in Terms(filter))
            if (row.SearchText.Contains(term, StringComparison.OrdinalIgnoreCase)) return true;
        return false;
    }

    public static List<string> Terms(string filter)
    {
        var terms = new List<string>();
        foreach (Match match in Regex.Matches(filter, "\"([^\"]+)\"|(\\S+)"))
        {
            var term = match.Groups[1].Success ? match.Groups[1].Value : match.Groups[2].Value;
            if (term.Length > 0) terms.Add(term.ToLowerInvariant());
        }
        return terms;
    }
}

/// <summary>Colours log rows by type (오류/경고/정보/상태), mirroring mockup `.lt-*`.</summary>
public sealed class KindBrushConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture) => (value as string) switch
    {
        "오류" => Resolve("ErrBrush", Color.FromRgb(0xB3, 0x26, 0x1E)),
        "경고" => Resolve("WarnBrush", Color.FromRgb(0xA0, 0x6B, 0x19)),
        "정보" => Resolve("InfoInkBrush", Color.FromRgb(0x3B, 0x5A, 0x86)),
        "상태" => Resolve("StatusInkBrush", Color.FromRgb(0x7A, 0x86, 0x99)),
        _ => Resolve("InkBrush", Color.FromRgb(0x1B, 0x28, 0x3D)),
    };

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        throw new NotSupportedException();

    internal static Brush Resolve(string key, Color fallback) =>
        Application.Current?.TryFindResource(key) as Brush ?? new SolidColorBrush(fallback);
}

/// <summary>Colours validation severities by the contract's `error|warning|info` values.</summary>
public sealed class SeverityBrushConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture) => (value as string) switch
    {
        "error" => KindBrushConverter.Resolve("ErrBrush", Color.FromRgb(0xB3, 0x26, 0x1E)),
        "warning" => KindBrushConverter.Resolve("WarnBrush", Color.FromRgb(0xA0, 0x6B, 0x19)),
        _ => KindBrushConverter.Resolve("InfoInkBrush", Color.FromRgb(0x3B, 0x5A, 0x86)),
    };

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        throw new NotSupportedException();
}
