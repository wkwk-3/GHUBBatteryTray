namespace GHUBBatteryTray
{
    public partial class Form1 : Form
    {
        private AppSettings _settings = new();
        private string _currentDevice = string.Empty;
        private bool _loading;

        public Form1()
        {
            InitializeComponent();

            Load += Form1_Load;

            cboDevice.SelectedIndexChanged += CboDevice_SelectedIndexChanged;
            btnAdd.Click += BtnAdd_Click;
            btnSave.Click += BtnSave_Click;
            btnClose.Click += BtnClose_Click;

            dgvNotification.CellContentClick += DgvNotification_CellContentClick;
        }

        private void Form1_Load(object? sender, EventArgs e)
        {
            _settings = AppSettings.Load();

            LoadDevices();
        }

        private void LoadDevices()
        {
            _loading = true;
            cboDevice.BeginUpdate();

            try
            {
                cboDevice.Items.Clear();

                HashSet<string> deviceNames = new(StringComparer.OrdinalIgnoreCase);

                try
                {
                    foreach (BatteryInfo battery in BatteryReader.GetBatteryList())
                    {
                        deviceNames.Add(battery.DeviceName);
                    }
                }
                catch (Exception ex)
                {
                    AppLogger.Error("設定画面のデバイス一覧取得に失敗しました。", ex);
                }

                foreach (string deviceName in _settings.DeviceNotifications.Keys)
                {
                    deviceNames.Add(deviceName);
                }

                foreach (string deviceName in deviceNames.OrderBy(x => x))
                {
                    cboDevice.Items.Add(deviceName);
                }

                if (cboDevice.Items.Count > 0)
                {
                    cboDevice.SelectedIndex = 0;
                    _currentDevice = cboDevice.SelectedItem?.ToString() ?? string.Empty;
                }
            }
            finally
            {
                cboDevice.EndUpdate();
                _loading = false;
            }

            LoadNotificationRulesForCurrentDevice();
        }

        private void CboDevice_SelectedIndexChanged(object? sender, EventArgs e)
        {
            if (_loading)
                return;

            SaveCurrentGridToSettings();
            _currentDevice = cboDevice.SelectedItem?.ToString() ?? string.Empty;
            LoadNotificationRulesForCurrentDevice();
        }

        private void LoadNotificationRulesForCurrentDevice()
        {
            dgvNotification.Rows.Clear();

            if (string.IsNullOrWhiteSpace(_currentDevice))
                return;

            if (!_settings.DeviceNotifications.TryGetValue(_currentDevice, out List<NotificationRule>? rules))
                return;

            foreach (NotificationRule rule in rules.OrderByDescending(x => x.Battery))
            {
                dgvNotification.Rows.Add(
                    rule.Enabled,
                    rule.Battery,
                    rule.BalloonTipIcon.ToString(),
                    rule.Message,
                    "×");
            }
        }

        private void BtnAdd_Click(object? sender, EventArgs e)
        {
            if (string.IsNullOrWhiteSpace(_currentDevice))
            {
                MessageBox.Show(
                    "通知設定を追加するデバイスがありません。",
                    "GHUB Battery",
                    MessageBoxButtons.OK,
                    MessageBoxIcon.Information);
                return;
            }

            dgvNotification.Rows.Add(
                true,
                20,
                ToolTipIcon.None.ToString(),
                "",
                "×");

            dgvNotification.CurrentCell =
                dgvNotification.Rows[dgvNotification.Rows.Count - 1].Cells["Battery"];

            dgvNotification.BeginEdit(true);
        }

        private void DgvNotification_CellContentClick(object? sender, DataGridViewCellEventArgs e)
        {
            if (e.RowIndex < 0)
                return;

            if (dgvNotification.Columns[e.ColumnIndex].Name != "Delete")
                return;

            dgvNotification.Rows.RemoveAt(e.RowIndex);
        }

        private void BtnSave_Click(object? sender, EventArgs e)
        {
            SaveCurrentGridToSettings();
            AppSettings.Save(_settings);
            AppLogger.Info("通知設定画面から設定を保存しました。");

            MessageBox.Show(
                "設定を保存しました。",
                "GHUB Battery",
                MessageBoxButtons.OK,
                MessageBoxIcon.Information);

            Close();
        }

        private void SaveCurrentGridToSettings()
        {
            if (string.IsNullOrWhiteSpace(_currentDevice))
                return;

            List<NotificationRule> rules = new();

            foreach (DataGridViewRow row in dgvNotification.Rows)
            {
                if (row.IsNewRow)
                    continue;

                if (row.Cells["Battery"].Value == null)
                    continue;

                NotificationRule rule = new()
                {
                    Enabled = Convert.ToBoolean(row.Cells["Enabled"].Value ?? true),
                    Battery = Convert.ToInt32(row.Cells["Battery"].Value),
                    BalloonTipIcon = ParseBalloonTipIcon(row.Cells["BalloonTipIcon"].Value),
                    Message = row.Cells["Message"].Value?.ToString() ?? ""
                };

                rules.Add(rule);
            }

            if (rules.Count == 0)
            {
                _settings.DeviceNotifications.Remove(_currentDevice);
            }
            else
            {
                _settings.DeviceNotifications[_currentDevice] = rules;
            }
        }

        private static ToolTipIcon ParseBalloonTipIcon(object? value)
        {
            string? iconName = value?.ToString();

            return Enum.TryParse(iconName, true, out ToolTipIcon icon)
                ? icon
                : ToolTipIcon.None;
        }

        private void BtnClose_Click(object? sender, EventArgs e)
        {
            Close();
        }
    }
}
