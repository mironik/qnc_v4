param(
  [string]$InitialPath = ''
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

$ColorBg = [System.Drawing.Color]::FromArgb(18, 18, 18)
$ColorFg = [System.Drawing.Color]::FromArgb(235, 235, 235)
$ColorPanel = [System.Drawing.Color]::FromArgb(28, 28, 28)
$ColorMeta = [System.Drawing.Color]::FromArgb(134, 134, 139)
$ColorAccent = [System.Drawing.Color]::FromArgb(255, 149, 0)
$ColorBtnBg = [System.Drawing.Color]::FromArgb(42, 42, 42)
$FontUi = New-Object System.Drawing.Font('Segoe UI', 11)
$PickMarker = 'QNC_PICK:'

function Style-Button($button, [bool]$primary) {
  $button.FlatStyle = [System.Windows.Forms.FlatStyle]::Flat
  $button.FlatAppearance.BorderSize = 1
  $button.Font = $FontUi
  $button.Height = 34
  $button.Width = 108
  $button.Margin = [System.Windows.Forms.Padding]::new(8, 0, 0, 0)
  if ($primary) {
    $button.BackColor = $ColorAccent
    $button.ForeColor = [System.Drawing.Color]::FromArgb(26, 26, 26)
    $button.FlatAppearance.BorderColor = $ColorAccent
  } else {
    $button.BackColor = $ColorBtnBg
    $button.ForeColor = $ColorFg
    $button.FlatAppearance.BorderColor = [System.Drawing.Color]::FromArgb(70, 70, 70)
  }
}

function Format-DriveLabel($drive) {
  $letter = $drive.Name.TrimEnd('\')
  $vol = [string]$drive.VolumeLabel
  if (-not ($vol -and $vol.Trim())) {
    try {
      $ld = Get-CimInstance -ClassName Win32_LogicalDisk -Filter ("DeviceID='" + $letter + "'") -ErrorAction SilentlyContinue
      if ($ld -and $ld.VolumeName) { $vol = [string]$ld.VolumeName }
    } catch { }
  }
  if ($vol -and $vol.Trim()) {
    return ($vol.Trim() + ' (' + $letter + ')')
  }
  return $letter
}

function Selection-Path($rawPath) {
  $p = [string]$rawPath
  if ([string]::IsNullOrWhiteSpace($p)) { return '' }
  if ((Test-Path -LiteralPath $p -PathType Leaf)) {
    return [System.IO.Path]::GetDirectoryName($p)
  }
  return $p
}

function Add-TreePlaceholder($parentNode) {
  $null = $parentNode.Nodes.Add([System.Windows.Forms.TreeNode]::new('...'))
}

function Add-DirectoryChildren($parentNode, $path) {
  $dirPath = [string]$path
  if ([string]::IsNullOrWhiteSpace($dirPath)) { return }
  if ($dirPath -match '^[A-Za-z]:$') { $dirPath = $dirPath + '\' }
  if (-not (Test-Path -LiteralPath $dirPath)) { return }
  try {
    foreach ($dir in [System.IO.Directory]::EnumerateDirectories($dirPath)) {
      $name = [System.IO.Path]::GetFileName($dir)
      if ([string]::IsNullOrWhiteSpace($name)) { continue }
      $child = $parentNode.Nodes.Add($name)
      $child.Tag = $dir
      Add-TreePlaceholder $child
    }
  } catch { }
}

function Ensure-NodeChildren($node) {
  if ($null -eq $node) { return }
  if ($node.Nodes.Count -eq 1 -and $node.Nodes[0].Text -eq '...') {
    $node.Nodes.Clear()
    Add-DirectoryChildren $node ([string]$node.Tag)
  }
}

function Enable-DarkTitleBar($targetForm) {
  try {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public class QncDwm {
  [DllImport("dwmapi.dll")]
  public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);
}
"@ -ErrorAction SilentlyContinue
    $useDark = 1
    [void][QncDwm]::DwmSetWindowAttribute($targetForm.Handle, 20, [ref]$useDark, 4)
  } catch { }
}

$form = New-Object System.Windows.Forms.Form
$form.Text = 'QNC - odaberi mapu'
$form.ClientSize = New-Object System.Drawing.Size(560, 420)
$form.MinimumSize = New-Object System.Drawing.Size(480, 360)
$form.BackColor = $ColorBg
$form.ForeColor = $ColorFg
$form.StartPosition = [System.Windows.Forms.FormStartPosition]::CenterScreen
$form.Font = $FontUi
$form.FormBorderStyle = [System.Windows.Forms.FormBorderStyle]::Sizable
$form.MaximizeBox = $false

$root = New-Object System.Windows.Forms.TableLayoutPanel
$root.Dock = [System.Windows.Forms.DockStyle]::Fill
$root.ColumnCount = 1
$root.RowCount = 3
$root.BackColor = $ColorBg
$root.Padding = [System.Windows.Forms.Padding]::new(12, 10, 12, 10)
$null = $root.ColumnStyles.Add((New-Object System.Windows.Forms.ColumnStyle([System.Windows.Forms.SizeType]::Percent, 100)))
$null = $root.RowStyles.Add((New-Object System.Windows.Forms.RowStyle([System.Windows.Forms.SizeType]::Absolute, 48)))
$null = $root.RowStyles.Add((New-Object System.Windows.Forms.RowStyle([System.Windows.Forms.SizeType]::Percent, 100)))
$null = $root.RowStyles.Add((New-Object System.Windows.Forms.RowStyle([System.Windows.Forms.SizeType]::Absolute, 44)))

$pathCell = New-Object System.Windows.Forms.Panel
$pathCell.Dock = [System.Windows.Forms.DockStyle]::Fill
$pathCell.BackColor = $ColorBg
$pathCell.Margin = [System.Windows.Forms.Padding]::new(0, 0, 0, 8)

$pathInner = New-Object System.Windows.Forms.TableLayoutPanel
$pathInner.Dock = [System.Windows.Forms.DockStyle]::Fill
$pathInner.ColumnCount = 2
$pathInner.RowCount = 1
$pathInner.BackColor = $ColorBg
$pathInner.Margin = [System.Windows.Forms.Padding]::new(0)
$pathInner.Padding = [System.Windows.Forms.Padding]::new(0)
$null = $pathInner.ColumnStyles.Add((New-Object System.Windows.Forms.ColumnStyle([System.Windows.Forms.SizeType]::AutoSize)))
$null = $pathInner.ColumnStyles.Add((New-Object System.Windows.Forms.ColumnStyle([System.Windows.Forms.SizeType]::Percent, 100)))

$pathLabel = New-Object System.Windows.Forms.Label
$pathLabel.Text = 'Putanja:'
$pathLabel.AutoSize = $true
$pathLabel.ForeColor = $ColorMeta
$pathLabel.Font = $FontUi
$pathLabel.Margin = [System.Windows.Forms.Padding]::new(0, 6, 10, 6)

$pathBox = New-Object System.Windows.Forms.TextBox
$pathBox.Dock = [System.Windows.Forms.DockStyle]::Fill
$pathBox.BackColor = $ColorPanel
$pathBox.ForeColor = $ColorFg
$pathBox.BorderStyle = [System.Windows.Forms.BorderStyle]::FixedSingle
$pathBox.Font = $FontUi
$pathBox.Margin = [System.Windows.Forms.Padding]::new(0, 4, 0, 4)
$pathBox.Height = 28

$null = $pathInner.Controls.Add($pathLabel, 0, 0)
$null = $pathInner.Controls.Add($pathBox, 1, 0)
$null = $pathCell.Controls.Add($pathInner)

$treeCell = New-Object System.Windows.Forms.Panel
$treeCell.Dock = [System.Windows.Forms.DockStyle]::Fill
$treeCell.BackColor = $ColorBg
$treeCell.Padding = [System.Windows.Forms.Padding]::new(0)
$treeCell.Margin = [System.Windows.Forms.Padding]::new(0, 8, 0, 8)

$tree = New-Object System.Windows.Forms.TreeView
$tree.Dock = [System.Windows.Forms.DockStyle]::Fill
$tree.BackColor = $ColorPanel
$tree.ForeColor = $ColorFg
$tree.BorderStyle = [System.Windows.Forms.BorderStyle]::FixedSingle
$tree.HideSelection = $false
$tree.ShowLines = $true
$tree.ShowPlusMinus = $true
$tree.ShowRootLines = $true
$tree.Font = $FontUi
$tree.ItemHeight = 24

$null = $treeCell.Controls.Add($tree)

$footerCell = New-Object System.Windows.Forms.Panel
$footerCell.Dock = [System.Windows.Forms.DockStyle]::Fill
$footerCell.BackColor = $ColorBg
$footerCell.Margin = [System.Windows.Forms.Padding]::new(0, 0, 0, 0)

$btnFlow = New-Object System.Windows.Forms.FlowLayoutPanel
$btnFlow.Dock = [System.Windows.Forms.DockStyle]::Right
$btnFlow.FlowDirection = [System.Windows.Forms.FlowDirection]::RightToLeft
$btnFlow.WrapContents = $false
$btnFlow.AutoSize = $true
$btnFlow.BackColor = $ColorBg

$btnCancel = New-Object System.Windows.Forms.Button
$btnCancel.Text = 'Odustani'
Style-Button $btnCancel $false

$btnOk = New-Object System.Windows.Forms.Button
$btnOk.Text = 'U redu'
Style-Button $btnOk $true

$btnNewFolder = New-Object System.Windows.Forms.Button
$btnNewFolder.Text = 'Nova mapa'
Style-Button $btnNewFolder $false
$btnNewFolder.Width = 120

$null = $btnFlow.Controls.Add($btnCancel)
$null = $btnFlow.Controls.Add($btnOk)

$leftFlow = New-Object System.Windows.Forms.FlowLayoutPanel
$leftFlow.Dock = [System.Windows.Forms.DockStyle]::Left
$leftFlow.AutoSize = $true
$leftFlow.BackColor = $ColorBg
$null = $leftFlow.Controls.Add($btnNewFolder)

$null = $footerCell.Controls.Add($btnFlow)
$null = $footerCell.Controls.Add($leftFlow)

$null = $root.Controls.Add($pathCell, 0, 0)
$null = $root.Controls.Add($treeCell, 0, 1)
$null = $root.Controls.Add($footerCell, 0, 2)

$null = $form.Controls.Add($root)

foreach ($drive in [System.IO.DriveInfo]::GetDrives()) {
  if (-not $drive.IsReady) { continue }
  $driveRoot = $drive.RootDirectory.FullName
  $node = $tree.Nodes.Add((Format-DriveLabel $drive))
  $node.Tag = $driveRoot
  Add-TreePlaceholder $node
}

$tree.Add_BeforeExpand({
  param($sender, $e)
  Ensure-NodeChildren $e.Node
})

$tree.Add_AfterExpand({
  param($sender, $e)
  $node = $e.Node
  if ($null -eq $node.Parent) {
    foreach ($driveNode in $tree.Nodes) {
      if ($driveNode -ne $node -and $driveNode.IsExpanded) {
        $driveNode.Collapse($false)
      }
    }
  }
})

$tree.Add_AfterSelect({
  param($sender, $e)
  if ($e.Node -and $e.Node.Tag) {
    $pathBox.Text = Selection-Path ([string]$e.Node.Tag)
    Ensure-NodeChildren $e.Node
    if ($null -eq $e.Node.Parent -and -not $e.Node.IsExpanded) {
      $e.Node.Expand()
    }
  }
})

$tree.Add_NodeMouseDoubleClick({
  param($sender, $e)
  if ($e.Node) {
    Ensure-NodeChildren $e.Node
    $e.Node.Expand()
  }
})

$btnOk.Add_Click({
  $picked = Selection-Path $pathBox.Text.Trim()
  if ([string]::IsNullOrWhiteSpace($picked)) { return }
  if (-not (Test-Path -LiteralPath $picked -PathType Container)) {
    try {
      New-Item -ItemType Directory -Path $picked -Force | Out-Null
    } catch {
      [System.Windows.Forms.MessageBox]::Show(
        $form,
        ('Ne mogu kreirati mapu:' + [Environment]::NewLine + $picked),
        'QNC',
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Error
      ) | Out-Null
      return
    }
  }
  $form.Tag = $picked
  $form.DialogResult = [System.Windows.Forms.DialogResult]::OK
  $form.Close()
})

$btnCancel.Add_Click({
  $form.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
  $form.Close()
})

$btnNewFolder.Add_Click({
  $base = Selection-Path $pathBox.Text.Trim()
  if (-not (Test-Path -LiteralPath $base)) { return }
  $name = 'Nova_mapa'
  $target = Join-Path $base $name
  $i = 1
  while (Test-Path -LiteralPath $target) {
    $target = Join-Path $base ('Nova_mapa_' + $i)
    $i++
  }
  try {
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    $pathBox.Text = $target
    if ($tree.SelectedNode) {
      $tree.SelectedNode.Nodes.Clear()
      Add-DirectoryChildren $tree.SelectedNode ([string]$tree.SelectedNode.Tag)
      $tree.SelectedNode.Expand()
    }
  } catch { }
})

$form.Add_Load({
  Enable-DarkTitleBar $form
  $start = $InitialPath.Trim()
  if ($start -and (Test-Path -LiteralPath $start)) {
    $pathBox.Text = Selection-Path $start
    return
  }
  if ($tree.Nodes.Count -gt 0) {
    $tree.SelectedNode = $tree.Nodes[0]
    $pathBox.Text = Selection-Path ([string]$tree.Nodes[0].Tag)
  }
})

$result = $form.ShowDialog()
if ($result -eq [System.Windows.Forms.DialogResult]::OK -and $form.Tag) {
  Write-Output ($PickMarker + [string]$form.Tag)
}
