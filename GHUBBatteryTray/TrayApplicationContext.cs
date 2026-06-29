using Timer = System.Windows.Forms.Timer;

namespace GHUBBatteryTray
{
    public class TrayApplicationContext : ApplicationContext
    {
        private readonly NotifyIcon _notifyIcon;
        private readonly Timer _timer;
        private readonly ContextMenuStrip _trayMenu = new();

        private readonly NotificationManager _notificationManager;

        private SettingsForm? _form;

        public TrayApplicationContext()
        {
            _notifyIcon = new NotifyIcon
            {
                Visible = true,
                Text = "GHUB Battery",
                ContextMenuStrip = _trayMenu
            };

            _notificationManager = new NotificationManager(_notifyIcon);

            _notifyIcon.MouseClick += NotifyIcon_MouseClick;
            _trayMenu.Opening += TrayMenu_Opening;

            _timer = new Timer();
            _timer.Interval = 30000;
            _timer.Tick += Timer_Tick;

            AppLogger.Info("アプリケーションを開始しました。");
            UpdateBattery();

            _timer.Start();
        }

        private void Timer_Tick(object? sender, EventArgs e)
        {
            UpdateBattery();
        }

        private void UpdateBattery()
        {
            try
            {
                AppSettings settings = AppSettings.Load();
                List<BatteryInfo> batteries = BatteryReader.GetBatteryList();

                UpdateTrayIcon(settings, batteries);
                CheckNotifications(settings, batteries);
            }
            catch (Exception ex)
            {
                AppLogger.Error("Battery情報の更新に失敗しました。", ex);
                _notifyIcon.Icon?.Dispose();
                _notifyIcon.Icon = TrayIconGenerator.CreateIcon(0);
                _notifyIcon.Text = "GHUB Battery 00";
            }
        }

        private void UpdateTrayIcon(AppSettings settings, List<BatteryInfo> batteries)
        {
            BatteryInfo? battery = batteries.Find(x => x.DeviceName.Equals(
                settings.TrayDisplayDevice,
                StringComparison.OrdinalIgnoreCase));

            int percentage = battery?.Percentage ?? 0;

            _notifyIcon.Icon?.Dispose();
            _notifyIcon.Icon = TrayIconGenerator.CreateIcon(percentage);

            _notifyIcon.Text = battery == null
                ? "GHUB Battery 00"
                : $"{battery.DeviceName} {battery.Percentage}%";
        }

        private void CheckNotifications(AppSettings settings, List<BatteryInfo> batteries)
        {
            foreach (BatteryInfo battery in batteries)
            {
                if (!settings.DeviceNotifications.TryGetValue(battery.DeviceName, out List<NotificationRule>? rules))
                    continue;

                _notificationManager.Check(
                    battery.DeviceName,
                    battery.Percentage,
                    rules);
            }
        }

        private void TrayMenu_Opening(object? sender, System.ComponentModel.CancelEventArgs e)
        {
            BuildTrayMenu();
        }

        private void BuildTrayMenu()
        {
            _trayMenu.Items.Clear();

            AppSettings settings = AppSettings.Load();
            List<BatteryInfo> batteries = new();

            try
            {
                batteries = BatteryReader.GetBatteryList();
            }
            catch (Exception ex)
            {
                AppLogger.Error("タスクトレイメニュー用のデバイス一覧取得に失敗しました。", ex);
            }

            ToolStripMenuItem noneItem = new("選択なし (00を表示)")
            {
                Checked = string.IsNullOrWhiteSpace(settings.TrayDisplayDevice)
            };
            noneItem.Click += (_, _) => SaveTrayDisplayDevice(string.Empty);
            _trayMenu.Items.Add(noneItem);

            foreach (BatteryInfo battery in batteries.OrderBy(x => x.DeviceName))
            {
                ToolStripMenuItem item = new($"{battery.DeviceName} ({battery.Percentage}%)")
                {
                    Checked = battery.DeviceName.Equals(settings.TrayDisplayDevice, StringComparison.OrdinalIgnoreCase)
                };

                string deviceName = battery.DeviceName;
                item.Click += (_, _) => SaveTrayDisplayDevice(deviceName);
                _trayMenu.Items.Add(item);
            }

            _trayMenu.Items.Add(new ToolStripSeparator());

            ToolStripMenuItem settingsItem = new("通知設定を開く");
            settingsItem.Click += (_, _) => ShowSettingsForm();
            _trayMenu.Items.Add(settingsItem);

            _trayMenu.Items.Add(new ToolStripSeparator());

            ToolStripMenuItem exitItem = new("プログラム終了");
            exitItem.Click += (_, _) => ExitApplication();
            _trayMenu.Items.Add(exitItem);
        }

        private void SaveTrayDisplayDevice(string deviceName)
        {
            AppSettings settings = AppSettings.Load();
            settings.TrayDisplayDevice = deviceName;
            AppSettings.Save(settings);
            AppLogger.Info(string.IsNullOrWhiteSpace(deviceName)
                ? "タスクトレイ表示対象を未選択にしました。"
                : $"タスクトレイ表示対象を変更しました。Device={deviceName}");
            UpdateBattery();
        }

        private void NotifyIcon_MouseClick(object? sender, MouseEventArgs e)
        {
            if (e.Button != MouseButtons.Left)
                return;

            ShowSettingsForm();
        }

        private void ShowSettingsForm()
        {
            if (_form == null || _form.IsDisposed)
            {
                _form = new SettingsForm();

                _form.FormClosed += (_, _) =>
                {
                    _form = null;
                };
            }

            _form.Show();
            _form.WindowState = FormWindowState.Normal;
            _form.Activate();
        }

        private void ExitApplication()
        {
            AppLogger.Info("タスクトレイメニューからプログラム終了が選択されました。");
            ExitThread();
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                AppLogger.Info("アプリケーションを終了します。");
                _timer.Stop();

                _notifyIcon.Visible = false;
                _notifyIcon.Icon?.Dispose();
                _notifyIcon.Dispose();
                _trayMenu.Dispose();

                _timer.Dispose();
            }

            base.Dispose(disposing);
        }
    }
}
