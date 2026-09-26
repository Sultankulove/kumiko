# Checks what UI Automation (screen readers on Windows) sees in a running prototype:
# the tree and its items, and where keyboard focus lands after moving the selection.
param([string]$Title = "Kumiko UI spike: iced")

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Focus {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
"@

$A = [System.Windows.Automation.AutomationElement]
$scope = [System.Windows.Automation.TreeScope]
$type = [System.Windows.Automation.ControlType]
function Condition($property, $value) {
    New-Object System.Windows.Automation.PropertyCondition($property, $value)
}

$window = $A::RootElement.FindFirst($scope::Children, (Condition $A::NameProperty $Title))
if (-not $window) { Write-Output "{`"window`":`"$Title`",`"found`":false}"; exit 2 }

$tree = $window.FindFirst($scope::Descendants, (Condition $A::ControlTypeProperty $type::Tree))
$items = $window.FindAll($scope::Descendants, (Condition $A::ControlTypeProperty $type::TreeItem))

[Win32Focus]::SetForegroundWindow([IntPtr]$window.Current.NativeWindowHandle) | Out-Null
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("{DOWN}{DOWN}{DOWN}")
Start-Sleep -Milliseconds 400
$focused = $A::FocusedElement

$result = [ordered]@{
    window      = $Title
    found       = $true
    tree        = [bool]$tree
    tree_items  = $items.Count
    focus_type  = $focused.Current.ControlType.ProgrammaticName
    focus_name  = $focused.Current.Name
}
$result | ConvertTo-Json -Compress
