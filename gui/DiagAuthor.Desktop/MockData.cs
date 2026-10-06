using System.Globalization;

namespace DiagAuthor.Desktop;

/// <summary>
/// Clearly-labelled demo data for mock mode. Every list entry carries the
/// performance-test / demo provenance in the UI's badge, banner and log; the
/// shapes match the engine's `tree.get` / `objects.query` payloads exactly so
/// the shell code path is identical in both modes.
/// </summary>
internal static class MockData
{
    private static readonly string[] Severities = { "오류", "경고", "정보" };

    public static int DidCount => 6;
    public static int DtcCount => 14;

    public static List<TreeItem> BuildTree()
    {
        var ecu = new TreeItem { Id = "ecu:demo", Kind = "ecu", Label = "Demo ECU", ChildCount = 8 };
        ecu.AddChild(new TreeItem { Id = "section:fault-memory", Kind = "section", Label = "고장 메모리" });
        ecu.AddChild(new TreeItem { Id = "did:all", Kind = "did", Label = "DID 개요", ChildCount = DidCount });

        var variant = new TreeItem { Id = "variant:base", Kind = "variant", Label = "Base Variant", ChildCount = 4 };
        variant.AddChild(new TreeItem { Id = "section:ident", Kind = "section", Label = "식별 특징" });
        variant.AddChild(new TreeItem { Id = "did:base", Kind = "did", Label = "DID", ChildCount = DidCount });
        variant.AddChild(new TreeItem { Id = "dtc:base", Kind = "dtc", Label = "DTC", ChildCount = DtcCount });
        variant.AddChild(new TreeItem { Id = "class:supported", Kind = "class", Label = "지원 진단 클래스", ChildCount = 9 });
        ecu.AddChild(variant);

        ecu.AddChild(new TreeItem { Id = "variant:all", Kind = "variant", Label = "변형", ChildCount = 3 });
        ecu.AddChild(new TreeItem { Id = "section:state", Kind = "section", Label = "상태" });
        ecu.AddChild(new TreeItem { Id = "data_type:all", Kind = "data_type", Label = "데이터 타입", ChildCount = 41 });
        ecu.AddChild(new TreeItem { Id = "service:all", Kind = "service", Label = "프로토콜 서비스", ChildCount = 31 });
        ecu.AddChild(new TreeItem { Id = "nrc:all", Kind = "nrc", Label = "부정 응답 코드", ChildCount = 40 });
        return new List<TreeItem> { ecu };
    }

    public static List<ObjectRow> Dids()
    {
        var rows = new List<ObjectRow>
        {
            Did("0xF190", "VIN", "식별", "17 바이트 · ASCII", "3개", "DID_F190", "정상"),
            Did("0xF191", "ECU Hardware Number", "식별", "12 바이트 · ASCII", "3개", "DID_F191", "정상"),
            Did("0xF186", "Active Diagnostic Session", "세션", "1 바이트 · 열거형", "2개", "DID_F186", "정상"),
            Did("0xF187", "Spare Part Number", "식별", "11 바이트 · ASCII", "3개", "DID_F187", "정상"),
            Did("0xF18C", "ECU Serial Number", "식별", "16 바이트 · ASCII", "1개", "DID_F18C", "정상"),
            Did("0x0100", "Brake Status", "브레이크", "4 바이트 · 8개 신호", "1개", "DID_0100", "주의"),
        };
        return rows;
    }

    public static List<ObjectRow> Dtcs()
    {
        return new List<ObjectRow>
        {
            Dtc("0x403012", "Pump Motor - Overtemperature", "오류", "모터 온도가 임계값을 초과했습니다", "3개", "DTC_403012", "확정"),
            Dtc("0x403013", "Pump Motor - Overcurrent", "오류", "모터 전류가 상한을 초과했습니다", "3개", "DTC_403013", "확정"),
            Dtc("0x40A101", "Brake Fluid Level Low", "경고", "브레이크 오일 레벨이 낮습니다", "2개", "DTC_40A101", "대기"),
            Dtc("0x40A102", "Brake Pad Wear", "경고", "브레이크 패드 마모 한계 근접", "2개", "DTC_40A102", "대기"),
            Dtc("0x40B011", "Wheel Speed Sensor FL - Circuit Open", "오류", "전륜 좌측 휠 속도 센서 회로 단선", "3개", "DTC_40B011", "확정"),
            Dtc("0x40B012", "Wheel Speed Sensor FR - Circuit Open", "오류", "전륜 우측 휠 속도 센서 회로 단선", "3개", "DTC_40B012", "확정"),
            Dtc("0x40B021", "Wheel Speed Sensor RL - Signal Invalid", "경고", "후륜 좌측 휠 속도 신호가 유효하지 않습니다", "2개", "DTC_40B021", "대기"),
            Dtc("0x40B022", "Wheel Speed Sensor RR - Signal Invalid", "경고", "후륜 우측 휠 속도 신호가 유효하지 않습니다", "2개", "DTC_40B022", "대기"),
            Dtc("0x40C031", "Steering Angle Sensor - Not Calibrated", "정보", "조향각 센서가 보정되지 않았습니다", "1개", "DTC_40C031", "대기"),
            Dtc("0x40C041", "Yaw Rate Sensor - Plausibility", "경고", "요레이트 센서 신호 개연성 오류", "2개", "DTC_40C041", "대기"),
            Dtc("0x40D051", "Battery Voltage Low", "경고", "배터리 전압이 하한 미만입니다", "3개", "DTC_40D051", "대기"),
            Dtc("0x40D052", "Battery Voltage High", "정보", "배터리 전압이 상한을 초과했습니다", "3개", "DTC_40D052", "대기"),
            Dtc("0x40E061", "TCM - Communication Lost", "오류", "변속기 제어 모듈 통신이 끊어졌습니다", "3개", "DTC_40E061", "확정"),
            Dtc("0x40E062", "TCM - Internal Failure", "오류", "변속기 제어 모듈 내부 고장", "3개", "DTC_40E062", "확정"),
        };
    }

    public static List<ValidationIssue> ValidationIssues() => new()
    {
        new ValidationIssue { Rule = "VAL-001", Severity = "error", Object = "DTC_40B011", Field = "qualifier", Message = "[목업] Qualifier가 명명 규칙 'DTC_<코드>'와 일치하지 않습니다." },
        new ValidationIssue { Rule = "VAL-020", Severity = "warning", Object = "DID_0100", Field = "name", Message = "[목업] DID 이름에 허용되지 않는 공백이 있습니다." },
        new ValidationIssue { Rule = "VAL-030", Severity = "info", Object = "DTC_40C031", Field = "severity", Message = "[목업] 심각도 '정보'는 배출가스 관련 규칙에서 제외됩니다." },
    };

    /// <summary>
    /// 20,000-row synthetic corpus for the virtualization / responsiveness test.
    /// Names, Qualifiers and the 상태 column mark the rows as test data.
    /// </summary>
    public static List<ObjectRow> PerformanceRows(int count)
    {
        var rows = new List<ObjectRow>(count);
        for (var i = 0; i < count; i++)
        {
            var code = i & 0xFFFFFF;
            var number = Views.Hex(code, 6);
            var name = "PERF-" + i.ToString("00000", CultureInfo.InvariantCulture) + " 합성 성능 행";
            var qualifier = "PERF_" + i.ToString("000000", CultureInfo.InvariantCulture);
            rows.Add(new ObjectRow
            {
                Id = "perf:" + i.ToString(CultureInfo.InvariantCulture),
                Kind = "dtc",
                Number = number,
                Name = name,
                Type = Severities[i % Severities.Length],
                Data = "가상화 성능 시험용 합성 데이터 (실제 진단 데이터 아님)",
                Shared = "독립",
                Qualifier = qualifier,
                Status = "시험",
                SearchText = ObjectRow.BuildSearchText(number, name, qualifier, "시험"),
            });
        }
        return rows;
    }

    private static ObjectRow Did(string number, string name, string type, string data, string shared, string qualifier, string status)
    {
        // Real DTC/DID numbering is decimal in the DB; keep the hex text for display
        // and derive the numeric id the same way the engine would.
        Views.TryParseHex(number, out var value);
        return new ObjectRow
        {
            Id = "did:" + value.ToString(CultureInfo.InvariantCulture),
            Kind = "did",
            Number = number,
            Name = name,
            Type = type,
            Data = data,
            Shared = shared,
            Qualifier = qualifier,
            Status = status,
            SearchText = ObjectRow.BuildSearchText(number, name, qualifier, type, data, status),
        };
    }

    private static ObjectRow Dtc(string number, string name, string severity, string description, string shared, string qualifier, string status)
    {
        Views.TryParseHex(number, out var value);
        return new ObjectRow
        {
            Id = "dtc:" + value.ToString(CultureInfo.InvariantCulture),
            Kind = "dtc",
            Number = number,
            Name = name,
            Type = severity,
            Data = description,
            Shared = shared,
            Qualifier = qualifier,
            Status = status,
            SearchText = ObjectRow.BuildSearchText(number, name, qualifier, severity, description, status),
        };
    }
}
