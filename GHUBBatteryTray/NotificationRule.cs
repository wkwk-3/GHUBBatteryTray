using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace GHUBBatteryTray
{
    public class NotificationRule
    {
        public bool Enabled { get; set; } = true;

        public int Battery { get; set; }

        public string Message { get; set; } = string.Empty;
    }
}
