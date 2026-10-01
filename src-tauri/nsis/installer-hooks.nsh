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

; Rename only links that actually target this application. Never touch foreign links.
!macro TeaCellMigrateShortcut oldPath newPath
  Push $0
  Push $1
  !insertmacro IsShortcutTarget "${oldPath}" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 = 1
    !insertmacro IsShortcutTarget "${newPath}" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $1
    ${If} $1 = 1
      Delete "${oldPath}"
    ${ElseIfNot} ${FileExists} "${newPath}"
      Rename "${oldPath}" "${newPath}"
    ${EndIf}
  ${EndIf}
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro TeaCellMigrateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$DESKTOP\${SHORTCUTNAME}.lnk"
  !insertmacro TeaCellMigrateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$SMPROGRAMS\${SHORTCUTNAME}.lnk"
  !insertmacro TeaCellMigrateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$SMPROGRAMS\$AppStartMenuFolder\${SHORTCUTNAME}.lnk"
  Push $0
  Push $1
  Push $2
  ; A new GUID on every install/reinstall, independent of the preserved user data.
  System::Call 'ole32::CoCreateGuid(g .r0) i.r1'
  ${If} $1 != 0
    Abort "无法创建安装标识，请重新运行安装程序。"
  ${EndIf}
  ClearErrors
  FileOpen $2 "$INSTDIR\installation-id.txt" w
  ${If} ${Errors}
    Abort "无法写入安装标识，请检查安装目录权限后重试。"
  ${EndIf}
  FileWrite $2 "$0$\r$\n"
  ${If} ${Errors}
    FileClose $2
    Abort "无法保存安装标识，请检查磁盘空间后重试。"
  ${EndIf}
  FileClose $2
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\installation-id.txt"
!macroend
