using System.Text.Json;

namespace GHUBBatteryTray
{
    public class AppSettings
    {
        private static readonly string DirectoryPath = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "GHUBBatteryTray");

        private static readonly string FilePath = Path.Combine(
            DirectoryPath,
            "settings.json");

        public static string SettingsDirectoryPath => DirectoryPath;

        /// <summary>
        /// タスクトレイに動的なBattery残量を表示する対象デバイス
        /// </summary>
        public string TrayDisplayDevice { get; set; } = string.Empty;

        /// <summary>
        /// 旧バージョン互換: タスクトレイに表示する対象デバイス
        /// </summary>
        public string SelectedDevice { get; set; } = string.Empty;

        /// <summary>
        /// 旧バージョン互換: 通知設定一覧
        /// </summary>
        public List<NotificationRule> Notifications { get; set; } = new();

        /// <summary>
        /// デバイスごとの通知設定一覧
        /// </summary>
        public Dictionary<string, List<NotificationRule>> DeviceNotifications { get; set; } = new(StringComparer.OrdinalIgnoreCase);

        /// <summary>
        /// 設定読み込み
        /// </summary>
        public static AppSettings Load()
        {
            if (!File.Exists(FilePath))
            {
                AppLogger.Info("設定ファイルがないため既定値を使用します。");
                return new AppSettings();
            }

            try
            {
                string json = File.ReadAllText(FilePath);

                AppSettings settings = JsonSerializer.Deserialize<AppSettings>(json)
                    ?? new AppSettings();

                settings.Normalize();
                return settings;
            }
            catch (Exception ex)
            {
                AppLogger.Error("設定ファイルの読み込みに失敗したため既定値を使用します。", ex);
                return new AppSettings();
            }
        }

        /// <summary>
        /// 設定保存
        /// </summary>
        public static void Save(AppSettings settings)
        {
            Directory.CreateDirectory(DirectoryPath);
            settings.Normalize();

            string json = JsonSerializer.Serialize(
                settings,
                new JsonSerializerOptions
                {
                    WriteIndented = true
                });

            File.WriteAllText(FilePath, json);
            AppLogger.Info("設定ファイルを保存しました。");
        }

        private void Normalize()
        {
            Notifications ??= new List<NotificationRule>();
            DeviceNotifications ??= new Dictionary<string, List<NotificationRule>>(StringComparer.OrdinalIgnoreCase);

            if (DeviceNotifications.Comparer != StringComparer.OrdinalIgnoreCase)
            {
                DeviceNotifications = new Dictionary<string, List<NotificationRule>>(
                    DeviceNotifications,
                    StringComparer.OrdinalIgnoreCase);
            }

            if (string.IsNullOrWhiteSpace(TrayDisplayDevice) && !string.IsNullOrWhiteSpace(SelectedDevice))
            {
                TrayDisplayDevice = SelectedDevice;
            }

            if (Notifications.Count > 0 && !string.IsNullOrWhiteSpace(SelectedDevice) &&
                !DeviceNotifications.ContainsKey(SelectedDevice))
            {
                DeviceNotifications[SelectedDevice] = Notifications;
            }

            SelectedDevice = TrayDisplayDevice;
            Notifications.Clear();
        }
    }
}
