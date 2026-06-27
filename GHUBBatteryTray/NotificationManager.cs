using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace GHUBBatteryTray
{
    public class NotificationManager
    {
        private readonly NotifyIcon _notifyIcon;
        private readonly NotificationHistory _history = new();
        private readonly String notificationReplaceToDeviceName = "{name}";
        private readonly String notificationReplaceToBatteryPer = "{battery}";
        private readonly String notificationReplaceToRn = "{rn}";

        public NotificationManager(NotifyIcon notifyIcon)
        {
            _notifyIcon = notifyIcon;
        }

        /// <summary>
        /// 通知判定
        /// </summary>
        /// <param name="batteryPercentage">現在のバッテリー残量</param>
        /// <param name="rules">通知設定一覧</param>
        public void Check(string deviceName, int batteryPercentage, List<NotificationRule> rules)
        {
            foreach (NotificationRule rule in rules.OrderByDescending(x => x.Battery))
            {
                if (!rule.Enabled)
                    continue;

                // 閾値以下になった
                if (batteryPercentage <= rule.Battery)
                {
                    if (!_history.IsNotified(deviceName, rule.Battery))
                    {
                        ShowNotification(deviceName, rule);

                        _history.SetNotified(deviceName, rule.Battery);
                    }
                }
                else
                {
                    // 閾値より上に戻ったので通知状態解除
                    _history.ClearNotified(deviceName, rule.Battery);
                }
            }
        }

        /// <summary>
        /// 通知表示
        /// </summary>
        private void ShowNotification(string deviceName, NotificationRule rule)
        {
            _notifyIcon.BalloonTipTitle = "GHUB Battery";

            _notifyIcon.BalloonTipText = rule.Message
                .Replace(notificationReplaceToDeviceName, deviceName)
                .Replace(notificationReplaceToBatteryPer, rule.Battery.ToString())
                .Replace(notificationReplaceToRn, "\r\n");

            AppLogger.Info($"通知を表示しました。Device={deviceName}, BatteryThreshold={rule.Battery}");

            _notifyIcon.BalloonTipIcon = ToolTipIcon.Warning;

            _notifyIcon.ShowBalloonTip(5000);
        }

        /// <summary>
        /// 通知履歴をリセット
        /// </summary>
        public void Reset()
        {
            _history.Clear();
        }
    }
}
