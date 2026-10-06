using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Globalization;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using Microsoft.Win32;

namespace DiagAuthor.Desktop;

public partial class MainWindow : Window
{
    private const int QueryLimit = 200;
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    private readonly EngineClient? engine;
    private readonly string engineError;
    private readonly bool forceMock;
    private readonly ObservableCollection<LogEntry> logEntries = new();
    private readonly ObservableCollection<ValidationIssue> issues = new();
    private readonly DispatcherTimer filterTimer = new() { Interval = TimeSpan.FromMilliseconds(220) };
    private readonly TaskCompletionSource readySource = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly List<string> pathHistory = new();

    private List<ObjectRow> allRows = new();
    private string currentKind = "dtc";
    private int didTotal;
    private int dtcTotal;
    private int logSequence;
    private int pathPosition = -1;
    private bool ready;
    private bool listOnly;
    private bool autoScrollLog = true;
    private double treeWidth = 230;
    private double dockHeight = 180;

    internal DataMode Mode { get; private set; } = DataMode.Mock;
    internal HelloInfo Hello { get; private set; } = new();
    internal EngineClient? Engine => engine;
    internal Task ReadyTask => readySource.Task;
    internal int RowCount => allRows.Count;
    internal int VisibleRowCount => ObjectGrid.Items.Count;
    internal int TreeRootCount => TreeRoot.Items.Count;
    internal int IssueCount => issues.Count;
    internal string CurrentKind => currentKind;
    internal int CurrentTab => PaneTabs.SelectedIndex;
    internal bool IsDockCollapsed => OutputDock.Visibility != Visibility.Visible;
    internal string StatusLine => StatusText.Text;

    public MainWindow(EngineClient? engine, string engineError, bool forceMock)
    {
        this.engine = engine;
        this.engineError = engineError;
        this.forceMock = forceMock;
        InitializeComponent();
        ObjectGrid.ItemsSource = new ObservableCollection<ObjectRow>();
        LogGrid.ItemsSource = logEntries;
        IssuesGrid.ItemsSource = issues;
        PaneIssuesGrid.ItemsSource = issues;
        filterTimer.Tick += (_, _) =>
        {
            filterTimer.Stop();
            ApplyFilter();
        };
        if (engine is not null) engine.Exited += OnEngineExited;
        Closed += (_, _) => engine?.Dispose();
        Loaded += async (_, _) => await InitializeAsync();
    }

    // =================================================================== startup
    private async Task InitializeAsync()
    {
        BuildColumns(currentKind);
        AddLog("정보", "시작", "DiagAuthor 데스크톱 셸 — 네이티브 WPF(XAML) · WebView2 없음.", null, null);

        if (engine is null)
        {
            EnterMockMode(forceMock
                ? "명령줄 --mock 로 엔진을 사용하지 않도록 지정했습니다."
                : "엔진 실행 파일을 찾지 못했습니다: " + engineError);
        }
        else
        {
            try
            {
                var hello = await engine.CallAsync("hello");
                Hello = new HelloInfo
                {
                    Protocol = hello["protocol"]?.GetValue<int>() ?? 0,
                    EngineVersion = hello["engine_version"]?.ToString() ?? "",
                    SchemaVersionMax = hello["schema_version_max"]?.GetValue<int>() ?? 0,
                };
                if (Hello.Protocol != Protocol.Version)
                    throw new InvalidOperationException($"프로토콜 버전이 다릅니다: 엔진 {Hello.Protocol}, GUI {Protocol.Version}");
                if (Hello.SchemaVersionMax <= 0)
                    throw new InvalidOperationException($"엔진이 schema_version_max를 보고하지 않았습니다: {hello.ToJsonString(Protocol.JsonOptions)}");

                Mode = DataMode.Engine;
                SetEngineBadge($"엔진 연결됨 · {Hello.Engine} {Hello.Version}", "OkBrush");
                ProtocolText.Text = $"protocol={Hello.Protocol}";
                SchemaText.Text = $"schema≤{Hello.SchemaVersionMax}";
                EngineVersionText.Text = Hello.EngineVersion;
                ToolbarModeText.Text = "네이티브 WPF · 엔진 연결됨";
                AddLog("상태", "엔진", $"hello 왕복 완료 · protocol={Hello.Protocol} · schema_version_max={Hello.SchemaVersionMax} · {Hello.EngineVersion}",
                    null, hello.ToJsonString(Protocol.JsonOptions));
                SetStatus($"엔진 hello 완료 · {Hello.EngineVersion} · protocol={Hello.Protocol} · schema_version_max={Hello.SchemaVersionMax}");
                await RefreshFromEngineAsync();
                await LoadCountsAsync();
            }
            catch (Exception ex)
            {
                EnterMockMode("엔진 연결 실패: " + ex.Message);
            }
        }

        ready = true;
        UpdateStatusCounts();
        readySource.TrySetResult();
    }

    private void EnterMockMode(string reason)
    {
        Mode = DataMode.Mock;
        Hello = new HelloInfo { Protocol = Protocol.Version, EngineVersion = "mock-engine 0.0.0-mock", SchemaVersionMax = Protocol.Version };
        MockBanner.Visibility = Visibility.Visible;
        MockBannerText.Text = "목업 모드 — " + reason + " · 표시되는 모든 행과 값은 실제 진단 데이터가 아닌 목업입니다.";
        Title = "DiagAuthor — Demo ECU (목업 모드)";
        SetEngineBadge("엔진 없음 — 목업 모드", "WarnBrush");
        ProtocolText.Text = "protocol=1 (목업)";
        SchemaText.Text = "schema=1 (목업)";
        EngineVersionText.Text = "mock-engine";
        ToolbarModeText.Text = "목업 모드 · 실데이터 아님";

        EcuCombo.ItemsSource = new[] { "Demo ECU" };
        EcuCombo.SelectedIndex = 0;
        PathBox.Text = "/Demo ECU/DTC";
        TreeRoot.ItemsSource = MockData.BuildTree();
        TreeEmpty.Visibility = Visibility.Collapsed;
        TreeNodeCount.Text = "노드 10";
        currentKind = "dtc";
        BuildColumns(currentKind);
        didTotal = MockData.DidCount;
        dtcTotal = MockData.DtcCount;
        allRows = MockData.Dtcs();
        ApplyFilter();
        issues.Clear();
        foreach (var issue in MockData.ValidationIssues()) issues.Add(issue);
        UpdateDockCount();
        AddLog("경고", "엔진", "목업 모드로 전환: " + reason, null, null);
        AddLog("정보", "프로젝트", "[목업] Demo ECU를 표시합니다 — tree.get / objects.query와 같은 응답 모양을 씁니다.", null, null);
        SetStatus("목업 모드 — 실제 진단 데이터가 아닌 목업을 표시합니다. 사유는 상단 배너를 참고하세요.");
    }

    internal async Task RefreshFromEngineAsync()
    {
        if (engine is null) return;
        var tree = await engine.CallAsync("tree.get");
        var nodes = TreeItem.FromJson(tree["nodes"]);
        TreeRoot.ItemsSource = nodes;
        TreeNodeCount.Text = $"노드 {CountNodes(nodes):N0}";
        TreeEmpty.Visibility = nodes.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        foreach (var node in nodes) node.IsExpanded = true;

        var status = await engine.CallAsync("project.status");
        var ecu = status["ecu"]?.ToString() ?? "";
        var open = status["open"]?.GetValue<bool>() == true;
        EcuCombo.ItemsSource = new[] { open ? ecu : "(열린 프로젝트 없음)" };
        EcuCombo.SelectedIndex = 0;
        PathBox.Text = string.IsNullOrEmpty(ecu) ? "/" : $"/{ecu}/DTC";
        AddLog("정보", "프로젝트", open
            ? $"프로젝트 열림 · ECU {ecu} · {status["path"]}"
            : "열린 프로젝트가 없습니다. 표의 행은 비어 있으며 [가져오기]로 CDD 미리보기를 실행할 수 있습니다.", null, null);
        await ReloadRowsAsync();
    }

    private static int CountNodes(IEnumerable<TreeItem> items)
    {
        var count = 0;
        foreach (var item in items)
        {
            count++;
            count += CountNodes(item.Children);
        }
        return count;
    }

    private async Task LoadCountsAsync()
    {
        if (engine is null) return;
        try
        {
            var did = await engine.CallAsync("objects.query", QueryArgs("did", "", 1, 0));
            didTotal = did["total"]?.GetValue<int>() ?? 0;
            var dtc = await engine.CallAsync("objects.query", QueryArgs("dtc", "", 1, 0));
            dtcTotal = dtc["total"]?.GetValue<int>() ?? 0;
        }
        catch (Exception ex)
        {
            ReportEngineError("objects.query", ex);
        }
    }

    private async Task ReloadRowsAsync()
    {
        try
        {
            if (Mode == DataMode.Mock)
            {
                allRows = currentKind == "did" ? MockData.Dids() : MockData.Dtcs();
            }
            else if (engine is not null)
            {
                var result = await engine.CallAsync("objects.query", QueryArgs(currentKind, "", QueryLimit, 0));
                allRows = ObjectRow.FromQuery(result, currentKind);
                var total = result["total"]?.GetValue<int>() ?? allRows.Count;
                if (currentKind == "did") didTotal = total;
                else dtcTotal = total;
            }
        }
        catch (Exception ex)
        {
            ReportEngineError("objects.query", ex);
            allRows = new List<ObjectRow>();
        }
        ApplyFilter();
    }

    private static JsonObject QueryArgs(string kind, string filter, int limit, int offset) => new()
    {
        ["kind"] = kind,
        ["filter"] = filter,
        ["limit"] = limit,
        ["offset"] = offset,
    };

    // =================================================================== list
    private void BuildColumns(string kind)
    {
        ObjectGrid.Columns.Clear();
        foreach (var spec in Views.ColumnsFor(kind))
        {
            var column = new DataGridTextColumn
            {
                Header = spec.Header,
                Binding = new Binding(spec.Binding),
                Width = new DataGridLength(spec.Width),
                ElementStyle = TextCellStyle(spec),
            };
            if (spec.Kind == "rowno")
            {
                column.CanUserSort = false;
                column.CanUserResize = false;
            }
            ObjectGrid.Columns.Add(column);
        }
    }

    private Style TextCellStyle(ColumnSpec spec)
    {
        var style = new Style(typeof(TextBlock));
        style.Setters.Add(new Setter(TextBlock.TextTrimmingProperty, TextTrimming.CharacterEllipsis));
        style.Setters.Add(new Setter(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center));
        if (spec.Numeric) style.Setters.Add(new Setter(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Right));
        switch (spec.Kind)
        {
            case "mono":
                style.Setters.Add(new Setter(TextBlock.FontFamilyProperty, FindResource("MonoFont")));
                break;
            case "rowno":
                style.Setters.Add(new Setter(TextBlock.ForegroundProperty, FindResource("GreyBrush")));
                style.Setters.Add(new Setter(TextBlock.FontSizeProperty, 11.0));
                style.Setters.Add(new Setter(TextBlock.PaddingProperty, new Thickness(0, 0, 5, 0)));
                break;
            case "dim":
                style.Setters.Add(new Setter(TextBlock.ForegroundProperty, FindResource("GreyBrush")));
                break;
            case "status":
                style.Setters.Add(new Setter(TextBlock.FontWeightProperty, FontWeights.SemiBold));
                foreach (var (value, brushKey) in new[] { ("오류", "ErrBrush"), ("경고", "WarnBrush"), ("주의", "WarnBrush"), ("시험", "TagBrush") })
                {
                    var trigger = new DataTrigger { Binding = new Binding(nameof(ObjectRow.Status)), Value = value };
                    trigger.Setters.Add(new Setter(TextBlock.ForegroundProperty, FindResource(brushKey)));
                    style.Triggers.Add(trigger);
                }
                break;
        }
        return style;
    }

    private void ApplyFilter()
    {
        var text = FilterBox.Text;
        var regex = RegexCheck.IsChecked == true;
        List<ObjectRow> filtered;
        try
        {
            filtered = allRows.Where(row => RowFilter.Matches(row, text, regex)).ToList();
            FilterBox.BorderBrush = (Brush)FindResource("Line2Brush");
            FilterBox.Background = (Brush)FindResource("PanelBrush");
        }
        catch (Exception ex) when (ex is ArgumentException or RegexMatchTimeoutException)
        {
            FilterBox.BorderBrush = (Brush)FindResource("ErrBrush");
            FilterBox.Background = (Brush)FindResource("ErrSoftBrush");
            SetStatus("정규식이 올바르지 않습니다: " + ex.Message);
            AddLog("경고", "필터", "정규식 오류로 필터를 적용하지 않았습니다: " + ex.Message, null, null);
            return;
        }
        AssignRows(filtered);
    }

    private void AssignRows(List<ObjectRow> rows)
    {
        for (var i = 0; i < rows.Count; i++) rows[i].RowNo = (i + 1).ToString(Inv);
        ObjectGrid.ItemsSource = new ObservableCollection<ObjectRow>(rows);
        ListLabel.Text = $"{Views.LabelFor(currentKind)} ({allRows.Count:N0})";
        if (rows.Count != allRows.Count)
        {
            FilterChip.Visibility = Visibility.Visible;
            FilterChipText.Text = $"{rows.Count:N0} / {allRows.Count:N0} 표시";
        }
        else
        {
            FilterChip.Visibility = Visibility.Collapsed;
        }
        PageInfo.Text = $"{rows.Count:N0}개 표시 · 헤더를 누르면 정렬, Ctrl+누르기로 2단 정렬 · 열 경계를 끌어 너비 조절 · 여러 행: Ctrl/Shift+클릭";
        if (ObjectGrid.SelectedIndex < 0 && rows.Count > 0) ObjectGrid.SelectedIndex = 0;
        UpdateStatusCounts();
        UpdatePane();
    }

    // =================================================================== selected-object pane
    private void UpdatePane()
    {
        PropertySheetPanel.Children.Clear();
        var selected = ObjectGrid.SelectedItems.OfType<ObjectRow>().ToList();
        var row = selected.Count > 0 ? selected[0] : allRows.Count > 0 ? allRows[0] : null;
        if (row is null)
        {
            AddSheetNote("표에서 객체를 고르면 속성을 보입니다.");
            PaneSubtitle.Text = "선택 없음";
            LayoutNote.Text = "표에서 DID를 고르면 데이터 배치를 보여줍니다. 실데이터 배치는 후속 마일스톤(M5)에서 엔진 op로 연결됩니다.";
            return;
        }
        if (selected.Count > 1) AddSheetNote($"{selected.Count:N0}개 선택 · 값이 다른 칸은 (여러 값)으로 표시합니다.");
        if (row.Kind == "did")
        {
            AddSheetRow("이름", row.Name);
            AddSheetRow("DID 번호", row.Number, mono: true);
            AddSheetRow("진단 클래스", row.Type);
            AddSheetRow("DID 데이터", row.Data);
            AddSheetRow("공유 변형", row.Shared);
            AddSheetRow("Qualifier", row.Qualifier);
            AddSheetRow("상태", row.Status);
        }
        else
        {
            var hasCode = Views.TryParseHex(row.Number, out var code);
            AddSheetRow("이름", row.Name);
            AddSheetRow("DTC 코드", hasCode ? $"{row.Number} · SAE {Views.SaeFromCode(code)}" : row.Number, mono: hasCode);
            AddSheetRow("심각도", row.Type);
            AddSheetRow("설명", row.Data);
            AddSheetRow("공유 변형", row.Shared);
            AddSheetRow("Qualifier", row.Qualifier);
            AddSheetRow("상태", row.Status);
        }
        PaneSubtitle.Text = selected.Count > 1 ? $"{selected.Count:N0}개 선택" : row.Name;
        LayoutNote.Text = row.Kind == "did"
            ? $"[셸 미리보기] '{row.Name}' · {row.Data} — 실제 바이트 배치는 후속 마일스톤(M5)에서 엔진 op로 연결됩니다."
            : "DTC에는 데이터 배치가 없습니다. DID를 선택하면 배치 자리 표시자를 보여줍니다.";
    }

    private void AddSheetNote(string text)
    {
        PropertySheetPanel.Children.Add(new TextBlock
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
            Padding = new Thickness(10, 8, 10, 8),
            Background = (Brush)FindResource("AccentSoftBrush"),
            Foreground = (Brush)FindResource("InfoInkBrush"),
            FontSize = 12,
        });
    }

    private void AddSheetRow(string label, string value, bool mono = false)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(190) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var labelBlock = new TextBlock
        {
            Text = label,
            Foreground = (Brush)FindResource("PathInkBrush"),
            Background = (Brush)FindResource("AltBrush"),
            Padding = new Thickness(9, 5, 8, 5),
            VerticalAlignment = VerticalAlignment.Stretch,
        };
        Grid.SetColumn(labelBlock, 0);
        grid.Children.Add(labelBlock);

        var valueBlock = new TextBlock
        {
            Text = value,
            Padding = new Thickness(9, 5, 8, 5),
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
        };
        if (mono)
        {
            valueBlock.FontFamily = (FontFamily)FindResource("MonoFont");
            valueBlock.Foreground = (Brush)FindResource("MutedBrush");
        }
        Grid.SetColumn(valueBlock, 1);
        grid.Children.Add(valueBlock);

        PropertySheetPanel.Children.Add(new Border
        {
            BorderBrush = (Brush)FindResource("RowLineBrush"),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Child = grid,
        });
    }

    // =================================================================== engine ops
    private async Task ImportPreviewAsync()
    {
        var dialog = new OpenFileDialog
        {
            Title = "CDD · CDDT 파일 열기",
            Filter = "CANdela 진단 문서 (*.cdd;*.cddt)|*.cdd;*.cddt|모든 파일 (*.*)|*.*",
            CheckFileExists = true,
        };
        if (dialog.ShowDialog(this) != true) return;

        if (Mode == DataMode.Mock)
        {
            AddLog("경고", "가져오기", $"[목업] cdd.preview({dialog.FileName}) — 엔진이 없어 실제 미리보기를 실행하지 않았습니다.", null, null);
            SetStatus("목업 모드에서는 CDD 미리보기를 실행할 수 없습니다.");
            return;
        }

        try
        {
            var preview = await engine!.CallAsync("cdd.preview", new JsonObject { ["path"] = dialog.FileName });
            var ecu = preview["ecu"]?.ToString() ?? "";
            var variants = preview["variants"]?.AsArray().Count ?? 0;
            var counts = preview["counts"];
            var didCount = counts?["did"]?.GetValue<int>() ?? 0;
            var dtcCount = counts?["dtc"]?.GetValue<int>() ?? 0;
            AddLog("정보", "가져오기", $"CDD 미리보기 · ECU {ecu} · 변형 {variants} · DID {didCount} · DTC {dtcCount}",
                null, preview.ToJsonString(Protocol.JsonOptions));
            SetStatus($"CDD 미리보기 완료 · {System.IO.Path.GetFileName(dialog.FileName)} · ECU {ecu} · DID {didCount} · DTC {dtcCount}");
        }
        catch (Exception ex)
        {
            ReportEngineError("cdd.preview", ex);
        }
    }

    private async Task RunValidationAsync()
    {
        issues.Clear();
        if (Mode == DataMode.Mock)
        {
            foreach (var issue in MockData.ValidationIssues()) issues.Add(issue);
            AddLog("경고", "검증", $"[목업] 검증 결과 {issues.Count}건 — 목업 규칙입니다.", null, null);
        }
        else if (engine is not null)
        {
            try
            {
                var result = await engine.CallAsync("validation.run");
                foreach (var issue in ValidationIssue.FromJson(result)) issues.Add(issue);
                AddLog(issues.Count == 0 ? "상태" : "정보", "검증", $"validation.run 완료 · 이슈 {issues.Count}건 (엔진 M1 규칙 집합)", null, null);
            }
            catch (Exception ex)
            {
                ReportEngineError("validation.run", ex);
            }
        }

        DockTabs.SelectedIndex = 1;
        PaneTabs.SelectedIndex = 3;
        ValidationSummary.Text = issues.Count == 0 ? "이슈 없음 — 엔진 규칙 집합이 아직 비어 있을 수 있습니다." : $"이슈 {issues.Count:N0}건";
        UpdateDockCount();
        SetStatus($"검증 완료 · 이슈 {issues.Count:N0}건");
    }

    private void OnEngineExited(string message)
    {
        if (Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) return;
        Dispatcher.Invoke(() =>
        {
            SetEngineBadge("엔진 종료됨", "ErrBrush");
            AddLog("오류", "엔진", message, null, engine?.RecentErrors());
            SetStatus("엔진 프로세스가 종료되었습니다. 상태를 다시 확인하세요.");
        });
    }

    private void ReportEngineError(string op, Exception ex)
    {
        AddLog("오류", "엔진", $"{op} 실패: {ex.Message}", null, ex.ToString());
        SetStatus($"엔진 오류({op}): {ex.Message}");
    }

    // =================================================================== chrome
    private void SetEngineBadge(string text, string brushKey)
    {
        EngineBadge.Background = (Brush)FindResource(brushKey);
        EngineBadgeText.Text = text;
    }

    private void SetStatus(string message) => StatusText.Text = message;

    private void AddLog(string kind, string origin, string message, string? linkKind = null, string? linkId = null, string? detail = null)
    {
        logEntries.Add(new LogEntry
        {
            No = ++logSequence,
            Kind = kind,
            Time = DateTime.Now.ToString("HH:mm:ss", Inv),
            Origin = origin,
            Message = message,
            Detail = detail ?? "",
            LinkText = linkKind is null ? "" : "↗",
            LinkKind = linkKind,
            LinkId = linkId,
        });
        if (autoScrollLog) LogGrid.ScrollIntoView(logEntries[^1]);
        UpdateDockCount();
    }

    private void UpdateDockCount() => DockCountText.Text = $"로그 {logEntries.Count:N0} · 이슈 {issues.Count:N0}";

    private void UpdateStatusCounts() => CountText.Text = $"DID {didTotal:N0} · DTC {dtcTotal:N0}";

    private void SetDockCollapsed(bool collapsed)
    {
        if (collapsed)
        {
            if (OutputDock.Visibility == Visibility.Visible && DockRow.ActualHeight > 20) dockHeight = DockRow.ActualHeight;
            OutputDock.Visibility = Visibility.Collapsed;
            DockSplitter.Visibility = Visibility.Collapsed;
            DockRow.Height = new GridLength(0);
            DockToggle.Content = "▸ 펼치기";
        }
        else
        {
            OutputDock.Visibility = Visibility.Visible;
            DockSplitter.Visibility = Visibility.Visible;
            DockRow.Height = new GridLength(dockHeight < 80 ? 180 : dockHeight);
            DockToggle.Content = "▾ 접기";
        }
    }

    private void SetListOnly(bool on)
    {
        listOnly = on;
        if (on)
        {
            treeWidth = TreeColumn.ActualWidth > 50 ? TreeColumn.ActualWidth : treeWidth;
            TreeColumn.MinWidth = 0;
            TreeColumn.Width = new GridLength(0);
            TreeSplitter.Visibility = Visibility.Collapsed;
            PaneRow.MinHeight = 0;
            PaneRow.Height = new GridLength(0);
            SetDockCollapsed(true);
            FullNote.Visibility = Visibility.Visible;
            ListOnlyButton.IsEnabled = false;
            SetStatus("목록 크게 보기 — Esc로 돌아갑니다.");
        }
        else
        {
            TreeColumn.MinWidth = 150;
            TreeColumn.Width = new GridLength(treeWidth < 150 ? 230 : treeWidth);
            TreeSplitter.Visibility = Visibility.Visible;
            PaneRow.MinHeight = 120;
            PaneRow.Height = new GridLength(1, GridUnitType.Star);
            SetDockCollapsed(false);
            FullNote.Visibility = Visibility.Collapsed;
            ListOnlyButton.IsEnabled = true;
        }
    }

    // =================================================================== events
    private async void Tree_SelectedItemChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (e.NewValue is not TreeItem item) return;
        PathBox.Text = item.PathText;
        if (item.Kind is "did" or "dtc")
        {
            currentKind = item.Kind;
            BuildColumns(currentKind);
            await ReloadRowsAsync();
            SetStatus($"{Views.LabelFor(currentKind)} 목록 — {item.PathText}");
            PushHistory(item.PathText);
        }
        else
        {
            SetStatus($"'{item.Label}' 노드 · 목록은 DID/DTC 노드에서 사용할 수 있습니다 (M3 셸 범위).");
        }
    }

    private void ObjectGrid_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        UpdatePane();
        if (ObjectGrid.SelectedItem is ObjectRow row)
            SetStatus($"{Views.LabelFor(row.Kind)} · {row.Number} {row.Name} · {row.Status}");
    }

    private void FilterBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (!ready) return;
        filterTimer.Stop();
        filterTimer.Start();
    }

    private void FilterApply_Click(object sender, RoutedEventArgs e)
    {
        filterTimer.Stop();
        ApplyFilter();
    }

    private void RegexCheck_Click(object sender, RoutedEventArgs e) => ApplyFilter();

    private void FilterChipClear_Click(object sender, RoutedEventArgs e)
    {
        FilterBox.Text = "";
        filterTimer.Stop();
        ApplyFilter();
    }

    private void Open_Click(object sender, RoutedEventArgs e) => _ = ImportPreviewAsync();

    private void ImportPreview_Click(object sender, RoutedEventArgs e) => _ = ImportPreviewAsync();

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        if (Mode == DataMode.Mock)
        {
            AddLog("경고", "파일", "[목업] 저장할 수 없습니다 — 엔진이 없습니다.", null, null);
            SetStatus("목업 모드에서는 저장할 수 없습니다.");
            return;
        }
        ScopeNote("프로젝트 저장", "M4");
    }

    private void SaveAs_Click(object sender, RoutedEventArgs e) => Save_Click(sender, e);

    private void Export_Click(object sender, RoutedEventArgs e) => ScopeNote("ODX 2.2.0 내보내기", "M6");

    private void Undo_Click(object sender, RoutedEventArgs e) => ScopeNote("실행 취소", "M4");

    private void Redo_Click(object sender, RoutedEventArgs e) => ScopeNote("다시 실행", "M4");

    private void Delete_Click(object sender, RoutedEventArgs e) => ScopeNote("삭제", "M4");

    private void ScopeNote(string feature, string milestone)
    {
        AddLog("정보", "범위", $"{feature}는 {milestone} 마일스톤에서 엔진 op로 연결됩니다.", null, null);
        SetStatus($"{feature}: {milestone} 범위입니다.");
    }

    private void Copy_Click(object sender, RoutedEventArgs e)
    {
        var rows = ObjectGrid.SelectedItems.OfType<ObjectRow>().ToList();
        if (rows.Count == 0)
        {
            SetStatus("복사할 행을 먼저 선택하세요.");
            return;
        }
        var text = string.Join(Environment.NewLine, rows.Select(r => string.Join('\t',
            r.RowNo, r.Number, r.Name, r.Type, r.Data, r.Shared, r.Qualifier, r.Status)));
        try
        {
            Clipboard.SetText(text);
            AddLog("정보", "편집", $"{rows.Count:N0}개 행을 클립보드에 복사했습니다.", null, null);
            SetStatus($"{rows.Count:N0}개 행을 클립보드에 복사했습니다.");
        }
        catch (System.Runtime.InteropServices.COMException ex)
        {
            AddLog("경고", "편집", "클립보드 복사 실패: " + ex.Message, null, null);
            SetStatus("클립보드 복사 실패: " + ex.Message);
        }
    }

    private void Validate_Click(object sender, RoutedEventArgs e) => _ = RunValidationAsync();

    private void ListOnly_Click(object sender, RoutedEventArgs e) => SetListOnly(!listOnly);

    private void DockToggle_Click(object sender, RoutedEventArgs e) => SetDockCollapsed(!IsDockCollapsed);

    private void ShowDock_Click(object sender, RoutedEventArgs e) => SetDockCollapsed(!IsDockCollapsed);

    private void ColumnInfo_Click(object sender, RoutedEventArgs e)
    {
        ColumnInfoTitle.Text = $"열 정보 · {Views.LabelFor(currentKind)} 목록";
        ColumnInfoGrid.ItemsSource = Views.ColumnInfo(currentKind);
        ColumnInfoOverlay.Visibility = Visibility.Visible;
    }

    private void ColumnInfoClose_Click(object sender, RoutedEventArgs e) => ColumnInfoOverlay.Visibility = Visibility.Collapsed;

    private void ClearLog_Click(object sender, RoutedEventArgs e)
    {
        logEntries.Clear();
        logSequence = 0;
        AddLog("상태", "로그", "작업 기록을 지웠습니다.", null, null);
    }

    private void LogAutoScroll_Click(object sender, RoutedEventArgs e)
    {
        autoScrollLog = !autoScrollLog;
        LogAutoScrollButton.Content = autoScrollLog ? "자동 스크롤 ✓" : "자동 스크롤";
    }

    private void ResetView_Click(object sender, RoutedEventArgs e)
    {
        if (listOnly) SetListOnly(false);
        TreeColumn.MinWidth = 150;
        TreeColumn.Width = new GridLength(230);
        DockRow.Height = new GridLength(180);
        dockHeight = 180;
        OutputDock.Visibility = Visibility.Visible;
        DockSplitter.Visibility = Visibility.Visible;
        DockToggle.Content = "▾ 접기";
        FilterBox.Text = "";
        RegexCheck.IsChecked = false;
        ApplyFilter();
        SetStatus("보기 배치를 초기화했습니다.");
    }

    private void Exit_Click(object sender, RoutedEventArgs e) => Close();

    private void Help_Click(object sender, RoutedEventArgs e) => MessageBox.Show(this,
        "단축키\n" +
        "  Ctrl+O  CDD · CDDT 미리보기\n" +
        "  Ctrl+S  저장(범위 안내)\n" +
        "  Ctrl+F  필터 입력으로 포커스\n" +
        "  F5 / F9  검증 실행\n" +
        "  Esc  목록 크게 보기 종료 · 오버레이 닫기\n\n" +
        "목록: 단어 OR 필터, \"따옴표\" 구절, 정규식 모드, 행 · 열 가상화(20,000행 시험).\n" +
        "하단 창: 작업 기록 · 검증 결과 (접기/펼치기).",
        "DiagAuthor 사용 안내", MessageBoxButton.OK, MessageBoxImage.Information);

    private void About_Click(object sender, RoutedEventArgs e)
    {
        var mode = Mode == DataMode.Engine ? "엔진 연결됨" : "목업 모드 (실데이터 아님)";
        MessageBox.Show(this,
            "DiagAuthor 데스크톱 셸 — 네이티브 WPF(XAML) · WebView2 없음\n" +
            $"데이터 모드: {mode}\n" +
            $"hello: protocol={Hello.Protocol} · schema_version_max={Hello.SchemaVersionMax}\n" +
            $"엔진: {Hello.EngineVersion}\n" +
            "IPC: JSON Lines (UTF-8, snake_case)",
            "DiagAuthor 정보", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private void PerfTest_Click(object sender, RoutedEventArgs e)
    {
        var loadMs = LoadPerformanceRowsForTest(20000);
        SetStatus($"성능 시험 · 20,000행 적재 {loadMs:N0} ms · 실현 행 {RealizedRowCountForTest():N0} (가상화) — 창을 스크롤해 응답을 확인하세요.");
    }

    private async void Window_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        var ctrl = Keyboard.Modifiers.HasFlag(ModifierKeys.Control);
        if (ctrl && e.Key == Key.F)
        {
            FilterBox.Focus();
            FilterBox.SelectAll();
            e.Handled = true;
        }
        else if (ctrl && e.Key == Key.O)
        {
            e.Handled = true;
            await ImportPreviewAsync();
        }
        else if (ctrl && e.Key == Key.S)
        {
            e.Handled = true;
            Save_Click(this, new RoutedEventArgs());
        }
        else if (e.Key is Key.F9 or Key.F5)
        {
            e.Handled = true;
            await RunValidationAsync();
        }
        else if (e.Key == Key.Escape && ColumnInfoOverlay.Visibility == Visibility.Visible)
        {
            ColumnInfoOverlay.Visibility = Visibility.Collapsed;
            e.Handled = true;
        }
        else if (e.Key == Key.Escape && listOnly)
        {
            SetListOnly(false);
            e.Handled = true;
        }
        else if (ctrl && e.Key == Key.C && Keyboard.FocusedElement is not TextBox)
        {
            e.Handled = true;
            Copy_Click(this, new RoutedEventArgs());
        }
    }

    private void Window_Closing(object sender, System.ComponentModel.CancelEventArgs e)
    {
        filterTimer.Stop();
        engine?.Dispose();
    }

    // =================================================================== path navigation
    private async void PathGo_Click(object sender, RoutedEventArgs e) => await NavigatePathAsync(PathBox.Text, true);

    private async void PathBox_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Enter) return;
        e.Handled = true;
        await NavigatePathAsync(PathBox.Text, true);
    }

    private async Task NavigatePathAsync(string text, bool recordHistory)
    {
        var trimmed = text.Trim();
        if (trimmed.StartsWith("npath:", StringComparison.OrdinalIgnoreCase)) trimmed = trimmed[6..];
        else if (trimmed.StartsWith("qpath:", StringComparison.OrdinalIgnoreCase)) trimmed = trimmed[6..];
        var segments = trimmed.Split('/', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var target = segments.Length == 0 ? "" : segments[^1];
        if (target.Length == 0)
        {
            SetStatus("이동할 경로를 입력하세요.");
            return;
        }

        if (TryFindTreeItem(TreeRoot.Items, target, out var treeItem) && treeItem is not null)
        {
            for (var parent = treeItem.Parent; parent is not null; parent = parent.Parent) parent.IsExpanded = true;
            treeItem.IsSelected = true;
            if (recordHistory) PushHistory(treeItem.PathText);
            SetStatus($"경로 이동 · {treeItem.PathText}");
            if (treeItem.Kind is "did" or "dtc")
            {
                currentKind = treeItem.Kind;
                BuildColumns(currentKind);
                await ReloadRowsAsync();
            }
            return;
        }

        var match = allRows.FirstOrDefault(r =>
            string.Equals(r.Name, target, StringComparison.OrdinalIgnoreCase) ||
            string.Equals(r.Qualifier, target, StringComparison.OrdinalIgnoreCase));
        if (match is not null)
        {
            var index = ObjectGrid.Items.IndexOf(match);
            if (index >= 0)
            {
                ObjectGrid.SelectedIndex = index;
                ObjectGrid.ScrollIntoView(match);
            }
            if (recordHistory) PushHistory(PathBox.Text.Trim());
            AddLog("상태", "탐색", $"경로 '{text}' → {match.Name}", match.Kind, match.Id);
            SetStatus($"경로 이동 · {match.Name} ({match.Number})");
            return;
        }

        SetStatus($"경로를 찾지 못했습니다: {text}");
    }

    private static bool TryFindTreeItem(System.Collections.IEnumerable items, string target, out TreeItem? found)
    {
        foreach (var entry in items)
        {
            if (entry is not TreeItem item) continue;
            if (string.Equals(item.Label, target, StringComparison.OrdinalIgnoreCase) ||
                string.Equals(item.Id, target, StringComparison.OrdinalIgnoreCase))
            {
                found = item;
                return true;
            }
            if (TryFindTreeItem(item.Children, target, out found)) return true;
        }
        found = null;
        return false;
    }

    private void PushHistory(string entry)
    {
        if (string.IsNullOrWhiteSpace(entry)) return;
        if (pathPosition >= 0 && pathPosition < pathHistory.Count && pathHistory[pathPosition] == entry) return;
        pathHistory.RemoveRange(pathPosition + 1, pathHistory.Count - pathPosition - 1);
        pathHistory.Add(entry);
        pathPosition = pathHistory.Count - 1;
    }

    private async void Back_Click(object sender, RoutedEventArgs e) => await StepHistoryAsync(-1);

    private async void Forward_Click(object sender, RoutedEventArgs e) => await StepHistoryAsync(1);

    private async Task StepHistoryAsync(int step)
    {
        var next = pathPosition + step;
        if (next < 0 || next >= pathHistory.Count) return;
        pathPosition = next;
        PathBox.Text = pathHistory[pathPosition];
        await NavigatePathAsync(PathBox.Text, false);
    }

    private void CopyPath_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Clipboard.SetText(PathBox.Text);
            SetStatus("이름 경로를 클립보드에 복사했습니다: " + PathBox.Text);
        }
        catch (System.Runtime.InteropServices.COMException ex)
        {
            SetStatus("클립보드 복사 실패: " + ex.Message);
        }
    }

    // =================================================================== test hooks (smoke test)
    internal async Task SelectKindForTestAsync(string kind)
    {
        currentKind = kind;
        BuildColumns(kind);
        await ReloadRowsAsync();
    }

    internal long ApplyFilterForTest(string text, bool regex)
    {
        var stopwatch = Stopwatch.StartNew();
        FilterBox.Text = text;
        RegexCheck.IsChecked = regex;
        filterTimer.Stop();
        ApplyFilter();
        stopwatch.Stop();
        return stopwatch.ElapsedMilliseconds;
    }

    internal void SelectTabForTest(int index) => PaneTabs.SelectedIndex = index;

    internal void SetDockCollapsedForTest(bool collapsed) => SetDockCollapsed(collapsed);

    internal long LoadPerformanceRowsForTest(int count)
    {
        currentKind = "dtc";
        BuildColumns("dtc");
        allRows = MockData.PerformanceRows(count);
        var stopwatch = Stopwatch.StartNew();
        AssignRows(allRows);
        ObjectGrid.UpdateLayout();
        stopwatch.Stop();
        SetStatus($"성능 시험: 합성 {count:N0}행 적재 + 레이아웃 {stopwatch.ElapsedMilliseconds:N0} ms (실제 진단 데이터 아님)");
        AddLog("상태", "성능", $"합성 {count:N0}행 적재 · 레이아웃 {stopwatch.ElapsedMilliseconds} ms · 행 가상화 Recycling", null, null);
        return stopwatch.ElapsedMilliseconds;
    }

    internal async Task<long> MeasureDispatcherAsync()
    {
        var stopwatch = Stopwatch.StartNew();
        await Dispatcher.InvokeAsync(() => { }, DispatcherPriority.Input);
        return stopwatch.ElapsedMilliseconds;
    }

    internal async Task<long> ScrollToEndAsync()
    {
        var stopwatch = Stopwatch.StartNew();
        if (ObjectGrid.Items.Count > 0) ObjectGrid.ScrollIntoView(ObjectGrid.Items[^1]);
        await Dispatcher.InvokeAsync(() => { }, DispatcherPriority.Input);
        return stopwatch.ElapsedMilliseconds;
    }

    internal int RealizedRowCountForTest() => CountVisualChildren<DataGridRow>(ObjectGrid);

    private static int CountVisualChildren<T>(DependencyObject root) where T : DependencyObject
    {
        var count = 0;
        var children = VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < children; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T) count++;
            count += CountVisualChildren<T>(child);
        }
        return count;
    }

    internal async Task RunValidationForTestAsync() => await RunValidationAsync();

    internal async Task CreateProjectForTestAsync(string path)
    {
        if (engine is null) return;
        await engine.CallAsync("project.create", new JsonObject
        {
            ["path"] = path,
            ["overwrite"] = true,
            ["name"] = "Smoke ECU",
            ["ecu"] = "Smoke ECU",
        });
        await RefreshFromEngineAsync();
    }
}
