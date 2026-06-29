namespace GHUBBatteryTray
{
    partial class SettingsForm
    {
        private System.ComponentModel.IContainer components = null;

        private System.Windows.Forms.ComboBox cboDevice;
        private System.Windows.Forms.Label lblReplaceHelp;
        private System.Windows.Forms.DataGridView dgvNotification;
        private System.Windows.Forms.Button btnAdd;
        private System.Windows.Forms.Button btnSave;
        private System.Windows.Forms.Button btnClose;

        protected override void Dispose(bool disposing)
        {
            if (disposing && (components != null))
            {
                components.Dispose();
            }

            base.Dispose(disposing);
        }

        private void InitializeComponent()
        {
            components = new System.ComponentModel.Container();

            cboDevice = new System.Windows.Forms.ComboBox();
            lblReplaceHelp = new System.Windows.Forms.Label();
            dgvNotification = new System.Windows.Forms.DataGridView();
            btnAdd = new System.Windows.Forms.Button();
            btnSave = new System.Windows.Forms.Button();
            btnClose = new System.Windows.Forms.Button();

            ((System.ComponentModel.ISupportInitialize)(dgvNotification)).BeginInit();

            SuspendLayout();

            //
            // cboDevice
            //
            cboDevice.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            cboDevice.Location = new System.Drawing.Point(12, 12);
            cboDevice.Name = "cboDevice";
            cboDevice.Size = new System.Drawing.Size(420, 28);

            //
            // lblReplaceHelp
            //
            lblReplaceHelp.AutoSize = true;
            lblReplaceHelp.Font = new System.Drawing.Font("Yu Gothic UI", 8F);
            lblReplaceHelp.Location = new System.Drawing.Point(438, 16);
            lblReplaceHelp.Name = "lblReplaceHelp";
            lblReplaceHelp.Size = new System.Drawing.Size(329, 13);
            lblReplaceHelp.Text = "リプレイス: {name}=デバイス名, {battery}=バッテリー残量, {rn}=改行";

            //
            // dgvNotification
            //
            dgvNotification.AllowUserToAddRows = false;
            dgvNotification.AllowUserToDeleteRows = false;
            dgvNotification.AllowUserToResizeRows = false;
            dgvNotification.RowHeadersVisible = false;
            dgvNotification.SelectionMode = System.Windows.Forms.DataGridViewSelectionMode.FullRowSelect;
            dgvNotification.MultiSelect = false;
            dgvNotification.Location = new System.Drawing.Point(12, 55);
            dgvNotification.Name = "dgvNotification";
            dgvNotification.Size = new System.Drawing.Size(760, 300);
            dgvNotification.AutoSizeColumnsMode = System.Windows.Forms.DataGridViewAutoSizeColumnsMode.Fill;

            dgvNotification.Columns.Add(new System.Windows.Forms.DataGridViewCheckBoxColumn()
            {
                Name = "Enabled",
                HeaderText = "ON",
                Width = 50,
                FillWeight = 15
            });

            dgvNotification.Columns.Add(new System.Windows.Forms.DataGridViewTextBoxColumn()
            {
                Name = "Battery",
                HeaderText = "Battery(%)",
                Width = 80,
                FillWeight = 20
            });

            dgvNotification.Columns.Add(new System.Windows.Forms.DataGridViewComboBoxColumn()
            {
                Name = "BalloonTipIcon",
                HeaderText = "通知アイコン",
                DataSource = Enum.GetNames(typeof(System.Windows.Forms.ToolTipIcon)),
                FillWeight = 35
            });

            dgvNotification.Columns.Add(new System.Windows.Forms.DataGridViewTextBoxColumn()
            {
                Name = "Message",
                HeaderText = "通知内容",
                FillWeight = 100
            });

            dgvNotification.Columns.Add(new System.Windows.Forms.DataGridViewButtonColumn()
            {
                Name = "Delete",
                HeaderText = "",
                Text = "×",
                UseColumnTextForButtonValue = true,
                Width = 50,
                FillWeight = 15
            });

            //
            // btnAdd
            //
            btnAdd.Location = new System.Drawing.Point(12, 370);
            btnAdd.Name = "btnAdd";
            btnAdd.Size = new System.Drawing.Size(120, 35);
            btnAdd.Text = "＋追加";
            btnAdd.UseVisualStyleBackColor = true;

            //
            // btnSave
            //
            btnSave.Location = new System.Drawing.Point(572, 370);
            btnSave.Name = "btnSave";
            btnSave.Size = new System.Drawing.Size(95, 35);
            btnSave.Text = "保存";
            btnSave.UseVisualStyleBackColor = true;

            //
            // btnClose
            //
            btnClose.Location = new System.Drawing.Point(677, 370);
            btnClose.Name = "btnClose";
            btnClose.Size = new System.Drawing.Size(95, 35);
            btnClose.Text = "閉じる";
            btnClose.UseVisualStyleBackColor = true;

            //
            // Form1
            //
            AutoScaleDimensions = new System.Drawing.SizeF(8F, 20F);
            AutoScaleMode = System.Windows.Forms.AutoScaleMode.Font;
            ClientSize = new System.Drawing.Size(784, 421);

            Controls.Add(cboDevice);
            Controls.Add(lblReplaceHelp);
            Controls.Add(dgvNotification);
            Controls.Add(btnAdd);
            Controls.Add(btnSave);
            Controls.Add(btnClose);

            FormBorderStyle = System.Windows.Forms.FormBorderStyle.FixedDialog;
            MaximizeBox = false;
            MinimizeBox = false;
            StartPosition = System.Windows.Forms.FormStartPosition.CenterScreen;
            Name = "Form1";
            Text = "GHUB Battery 設定";

            ((System.ComponentModel.ISupportInitialize)(dgvNotification)).EndInit();

            ResumeLayout(false);
            PerformLayout();
        }
    }
}
