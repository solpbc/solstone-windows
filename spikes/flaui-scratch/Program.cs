using System;
using System.Windows.Forms;

ApplicationConfiguration.Initialize();

var form = new Form
{
    Text = "FlaUI Session 1 Target",
    Width = 420,
    Height = 180,
    StartPosition = FormStartPosition.CenterScreen
};

var status = new TextBox
{
    Name = "StatusBox",
    Text = "Ready",
    ReadOnly = true,
    Left = 24,
    Top = 24,
    Width = 240
};

var button = new Button
{
    Name = "PrimaryButton",
    Text = "Click",
    Left = 24,
    Top = 64,
    Width = 120,
    Height = 36
};
button.Click += (_, _) => status.Text = "Clicked";

form.Controls.Add(status);
form.Controls.Add(button);
Application.Run(form);
