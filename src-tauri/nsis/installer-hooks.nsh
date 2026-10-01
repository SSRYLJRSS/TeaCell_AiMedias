; Tauri NSIS keys are derived from productName. Explicitly handle the old Chinese name
; before copying new files; never touch the separate application-data directory.
!macro NSIS_HOOK_PREINSTALL
  Push $R0
  Push $R1
  Push $R2
  ReadRegStr $R2 SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\茶馆" "UninstallString"
  StrCmp $R2 "" legacy_done
  ReadRegStr $R0 SHCTX "Software\TeaCell\茶馆" ""
  StrCmp $R0 "" legacy_refuse
  ReadRegStr $R1 SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\茶馆" "Publisher"
  StrCmp $R1 "TeaCell" 0 legacy_refuse
  IfFileExists "$R0\uninstall.exe" 0 legacy_refuse
  ; A rename migration requires user confirmation; silent installs fail closed.
  IfSilent legacy_refuse
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Replace the existing 茶馆 installation with TeaCell AI Media Manager? Your library and application data will be kept.$\r$\n是否替换已有茶馆安装？素材库和软件数据将保留。" IDYES legacy_replace
  Abort
legacy_replace:
  ClearErrors
  ExecWait '"$R0\uninstall.exe" /S _?=$R0' $R2
  IfErrors legacy_refuse
  StrCmp $R2 0 0 legacy_refuse
  ; Do not proceed if the old uninstaller left its registration behind.
  ReadRegStr $R1 SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\茶馆" "UninstallString"
  StrCmp $R1 "" legacy_done
legacy_refuse:
  MessageBox MB_OK|MB_ICONSTOP "The previous 茶馆 installation could not be safely replaced. Uninstall it normally, then run this installer again. Do not delete your library or application data.$\r$\n无法安全替换旧安装。请先正常卸载旧版后重试，不要删除素材库或软件数据。" /SD IDOK
  Abort
legacy_done:
  Pop $R2
  Pop $R1
  Pop $R0
!macroend
