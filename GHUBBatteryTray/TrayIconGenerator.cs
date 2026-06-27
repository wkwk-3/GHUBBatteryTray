using System;
using System.Collections.Generic;
using System.Drawing.Drawing2D;
using System.Drawing.Text;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;

namespace GHUBBatteryTray
{
    public static class TrayIconGenerator
    {
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool DestroyIcon(IntPtr hIcon);

        public static Icon CreateIcon(int percentage)
        {
            percentage = Math.Clamp(percentage, 0, 99);

            const int canvasSize = 32;

            using Bitmap bitmap = new(canvasSize, canvasSize);

            using (Graphics g = Graphics.FromImage(bitmap))
            {
                g.SmoothingMode = SmoothingMode.HighQuality;
                g.InterpolationMode = InterpolationMode.HighQualityBicubic;
                g.PixelOffsetMode = PixelOffsetMode.HighQuality;
                g.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;

                g.Clear(Color.Transparent);

                using Font font = new(
                    "Bahnschrift",
                    16f,
                    FontStyle.Regular,
                    GraphicsUnit.Pixel);

                string text = percentage.ToString("00");

                SizeF size = g.MeasureString(text, font);

                float x = (canvasSize - size.Width) / 2f;
                float y = (canvasSize - size.Height) / 2f - 1;

                Brush brush = percentage switch
                {
                    <= 20 => Brushes.Red,
                    <= 50 => Brushes.DarkOrange,
                    _ => Brushes.Black
                };

                using GraphicsPath path = new();

                path.AddString(
                    text,
                    font.FontFamily,
                    (int)font.Style,
                    g.DpiY * font.Size / 72f,
                    new PointF(x, y),
                    StringFormat.GenericDefault);

                using Pen outlinePen = new(Color.White, 2)
                {
                    LineJoin = LineJoin.Round
                };

                g.FillPath(brush, path);
                g.DrawPath(outlinePen, path);
            }

            using Bitmap iconBitmap = new(bitmap, new Size(16, 16));

            IntPtr hIcon = iconBitmap.GetHicon();

            try
            {
                return (Icon)Icon.FromHandle(hIcon).Clone();
            }
            finally
            {
                DestroyIcon(hIcon);
            }
        }
    }
}
