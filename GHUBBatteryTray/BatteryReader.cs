using Microsoft.Data.Sqlite;
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Threading.Tasks;

namespace GHUBBatteryTray
{
    public static class BatteryReader
    {
        private static readonly string DbPath = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "LGHUB",
            "settings.db");

        private static readonly Regex BatteryRegex =
            new(@"^battery/.+/percentage$", RegexOptions.Compiled);

        public static List<BatteryInfo> GetBatteryList()
        {
            if (!File.Exists(DbPath))
            {
                throw new FileNotFoundException(
                    $"settings.db が見つかりません。\n{DbPath}");
            }

            byte[] blob;

            using (var connection = new SqliteConnection(
                $"Data Source={DbPath};Mode=ReadOnly"))
            {
                connection.Open();

                using var command = connection.CreateCommand();

                // 必要に応じてWHERE句は変更してください
                command.CommandText = @"
                    SELECT file
                    FROM data
                    LIMIT 1;";

                object? result = command.ExecuteScalar();

                if (result == null || result == DBNull.Value)
                {
                    throw new Exception("JSONデータが取得できませんでした。");
                }

                blob = (byte[])result;
            }

            string json = Encoding.UTF8.GetString(blob);

            using JsonDocument document = JsonDocument.Parse(json);

            List<BatteryInfo> batteries = new();

            foreach (JsonProperty property in document.RootElement.EnumerateObject())
            {
                if (!BatteryRegex.IsMatch(property.Name))
                    continue;

                string[] split = property.Name.Split('/');

                if (split.Length != 3)
                    continue;

                if (!property.Value.TryGetProperty("percentage", out JsonElement percentageElement))
                    continue;

                batteries.Add(new BatteryInfo
                {
                    DeviceName = split[1],
                    Percentage = percentageElement.GetInt32()
                });
            }

            return batteries;
        }

        public static BatteryInfo? GetBattery(string deviceName)
        {
            return GetBatteryList()
                .Find(x => x.DeviceName.Equals(
                    deviceName,
                    StringComparison.OrdinalIgnoreCase));
        }
    }
}
