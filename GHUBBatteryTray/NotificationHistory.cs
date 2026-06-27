namespace GHUBBatteryTray
{
    public class NotificationHistory
    {
        private readonly HashSet<string> _notifiedKeys = new(StringComparer.OrdinalIgnoreCase);

        public bool IsNotified(string deviceName, int battery)
        {
            return _notifiedKeys.Contains(CreateKey(deviceName, battery));
        }

        public void SetNotified(string deviceName, int battery)
        {
            _notifiedKeys.Add(CreateKey(deviceName, battery));
        }

        public void ClearNotified(string deviceName, int battery)
        {
            _notifiedKeys.Remove(CreateKey(deviceName, battery));
        }

        public void Clear()
        {
            _notifiedKeys.Clear();
        }

        private static string CreateKey(string deviceName, int battery)
        {
            return $"{deviceName}|{battery}";
        }
    }
}
