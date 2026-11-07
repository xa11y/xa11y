# Native WPF fixture: AccessKit does not expose UIA Grid/Table or rich text.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationFramework
Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName WindowsBase
[xml]$xaml = @'
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="xa11y Windows Rich Fixture" Width="700" Height="650">
  <StackPanel Margin="16">
    <DataGrid x:Name="Grid" AutomationProperties.Name="Rich grid"
              AutoGenerateColumns="True" CanUserAddRows="False"
              EnableRowVirtualization="False" EnableColumnVirtualization="False"
              SelectionMode="Extended" Height="160" />
    <ListBox x:Name="Items" AutomationProperties.Name="Rich list" Height="90" SelectionMode="Multiple">
      <ListBoxItem IsSelected="True">Apple</ListBoxItem>
      <ListBoxItem>Banana</ListBoxItem>
      <ListBoxItem IsSelected="True">Cherry</ListBoxItem>
    </ListBox>
    <TextBox x:Name="Edit" AutomationProperties.Name="Edit text" AcceptsReturn="True" Height="90" />
    <RichTextBox x:Name="Rich" AutomationProperties.Name="Rich text" Height="160" IsReadOnly="True">
      <FlowDocument><Paragraph Margin="0"><Run xml:space="preserve">Hello </Run><Bold><Run xml:space="preserve">bold</Run></Bold><Italic><Run xml:space="preserve"> italic</Run></Italic><Run xml:space="preserve" TextDecorations="Underline" Foreground="Red"> red</Run><Run FontWeight="Bold">!</Run><Run xml:space="preserve"> tail</Run></Paragraph></FlowDocument>
    </RichTextBox>
    <Button AutomationProperties.Name="Plain button">Plain button</Button>
  </StackPanel>
</Window>
'@
$window = [Windows.Markup.XamlReader]::Load((New-Object System.Xml.XmlNodeReader $xaml))
$grid = $window.FindName('Grid')
# DataTable supplies real CLR properties for DataGrid's auto-generated columns.
$data = New-Object System.Data.DataTable
[void]$data.Columns.Add('Name')
[void]$data.Columns.Add('Role')
[void]$data.Rows.Add('Alice', 'Admin')
[void]$data.Rows.Add('Bob', 'User')
$grid.ItemsSource = $data.DefaultView
$edit = $window.FindName('Edit')
$edit.Text = 'Hello ' + [char]::ConvertFromUtf32(0x1f600) + " world`r`nSecond line"
$window.Add_ContentRendered({ $edit.Focus(); $edit.Select(6, 2) })
[void]$window.ShowDialog()
