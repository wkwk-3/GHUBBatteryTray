namespace GHUBBatteryTray
{
    public static class AppLogger
    {
        private static readonly object SyncRoot = new();
        private static readonly string LogPath = Path.Combine(AppSettings.SettingsDirectoryPath, "GHUBBatteryTray.log");

        public static void Info(string message)
        {
            Write("INFO", message);
        }

        public static void Error(string message, Exception ex)
        {
            Write("ERROR", $"{message} {ex.GetType().Name}: {ex.Message}");
        }

        private static void Write(string level, string message)
        {
            try
            {
                Directory.CreateDirectory(AppSettings.SettingsDirectoryPath);

                string line = $"{DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss.fff zzz} [{level}] {message}{Environment.NewLine}";

                lock (SyncRoot)
                {
                    File.AppendAllText(LogPath, line);
                }
            }
            catch
            {
                // ログ出力失敗でアプリ本体の動作を止めない
            }
        }
    }
}
