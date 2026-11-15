' Henry Chat Pro - Khoi chay an ban Release doc lap (khong hien cua so CMD)
Option Explicit
Dim sh, fso, appDir, psFile, cmd
Set sh = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
appDir = fso.GetParentFolderName(WScript.ScriptFullName)
psFile = appDir & "\scripts\launcher.ps1"

If Not fso.FileExists(psFile) Then
    MsgBox "Khong tim thay file: " & psFile, 16, "Henry Chat Pro"
    WScript.Quit 1
End If

sh.CurrentDirectory = appDir
cmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File """ & psFile & """"
sh.Run cmd, 0, False
