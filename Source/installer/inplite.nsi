; InpLite installer (NSIS 3). Per-user install, no administrator rights needed.
; Build:  makensis installer\inplite.nsi   (expects dist\inplite.exe)
; An existing InpLite installation (any version) is uninstalled first; an installation of
; version 0.2.0, which was briefly called "Abapad++", is removed as well.
; 64-bit installer (the application is x64 only)
Target amd64-unicode
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"

!define APPNAME "InpLite"
!define VERSION "1.2.0"
!define EXE "InpLite.exe"
!define PROGID "InpLite.inp"
!define OLD_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Abapad++"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

Name "${APPNAME} ${VERSION}"
OutFile "..\dist\InpLite-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${APPNAME}"
InstallDirRegKey HKCU "Software\${APPNAME}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APPNAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "${APPNAME}"
VIAddVersionKey /LANG=1033 "FileDescription" "${APPNAME} setup"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "MIT License"

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\README.txt"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE.txt"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; English only (the installer does not follow the Windows display language)
!insertmacro MUI_LANGUAGE "English"

LangString SEC_MAIN ${LANG_ENGLISH} "InpLite (required)"
LangString SEC_DESK ${LANG_ENGLISH} "Desktop shortcut"
LangString SEC_OPEN ${LANG_ENGLISH} "Right-click menu entry"
LangString DESC_MAIN ${LANG_ENGLISH} "The editor (about 7 MB)."
LangString DESC_DESK ${LANG_ENGLISH} "Create a shortcut on the desktop."
LangString DESC_OPEN ${LANG_ENGLISH} "'Edit with InpLite' in the right-click menu and InpLite in 'Open with' for .inp files (Windows 11: under 'Open with' and 'Show more options'); InpLite also in 'Open with' for .dat, .msg, .sta and .log. The default program is not changed."
LangString SEC_CLASSIC ${LANG_ENGLISH} "Win 11 full right-click"
LangString DESC_CLASSIC ${LANG_ENGLISH} "Shows the full (Windows 10 style) right-click menu in Explorer, so 'Edit with InpLite' appears without 'Show more options'. For your account, after sign-out. Undone on uninstall."

Section "!$(SEC_MAIN)" SecMain
  SectionIn RO
  ; InpLite must not be running (its program file would be locked)
  ReadRegStr $1 HKCU "${UNINST_KEY}" "InstallLocation"
  ${If} $1 == ""
    StrCpy $1 "$INSTDIR"
  ${EndIf}
  ${If} ${FileExists} "$1\${EXE}"
    retry_running:
    ClearErrors
    FileOpen $0 "$1\${EXE}" a
    ${If} ${Errors}
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "InpLite is running. Close all InpLite windows and press Retry." /SD IDCANCEL IDRETRY retry_running
      Abort "InpLite is running."
    ${EndIf}
    FileClose $0
  ${EndIf}
  ; remove the installed version first (files, menu entries, shortcuts, registry), so that
  ; nothing of an older version stays behind
  ReadRegStr $1 HKCU "${UNINST_KEY}" "InstallLocation"
  ${If} $1 != ""
  ${AndIf} ${FileExists} "$1\Uninstall.exe"
    DetailPrint "Removing the installed version ($1)"
    ExecWait '"$1\Uninstall.exe" /S _?=$1'
    Delete "$1\Uninstall.exe"
    RMDir "$1"
  ${EndIf}
  ; remove an installation of 0.2.0 ("Abapad++"): files, menu entries, shortcuts
  ReadRegStr $1 HKCU "${OLD_UNINST_KEY}" "InstallLocation"
  ${If} $1 != ""
  ${AndIf} ${FileExists} "$1\Uninstall.exe"
    ExecWait '"$1\Uninstall.exe" /S _?=$1'
    Delete "$1\Uninstall.exe"
    RMDir "$1"
  ${EndIf}
  SetOutPath "$INSTDIR"
  File "/oname=${EXE}" "..\dist\inplite.exe"
  File "/oname=README.txt" "..\README.md"
  File "..\LICENSE.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APPNAME}.lnk" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${APPNAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE},0"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" 7000
SectionEnd

Section "$(SEC_DESK)" SecDesktop
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXE}"
SectionEnd

Section "$(SEC_OPEN)" SecOpenWith
  ; "Open with" list
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}" "FriendlyAppName" "${APPNAME}"
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\SupportedTypes" ".inp" ""
  ; Abaqus output files: InpLite is offered under "Open with" (no default change, no menu entry)
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\SupportedTypes" ".dat" ""
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\SupportedTypes" ".msg" ""
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\SupportedTypes" ".sta" ""
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\SupportedTypes" ".log" ""
  WriteRegStr HKCU "Software\Classes\.sta\OpenWithList\${EXE}" "" ""
  WriteRegStr HKCU "Software\Classes\Applications\${EXE}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
  WriteRegStr HKCU "Software\Classes\${PROGID}" "" "Abaqus input file"
  WriteRegStr HKCU "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\${EXE},0"
  WriteRegStr HKCU "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
  WriteRegStr HKCU "Software\Classes\.inp\OpenWithProgids" "${PROGID}" ""
  ; right-click "Edit with InpLite" (does not change the default program)
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\.inp\shell\InpLite" "" "Edit with InpLite"
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\.inp\shell\InpLite" "Icon" "$INSTDIR\${EXE},0"
  WriteRegStr HKCU "Software\Classes\SystemFileAssociations\.inp\shell\InpLite\command" "" '"$INSTDIR\${EXE}" "%1"'
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section /o "$(SEC_CLASSIC)" SecClassic
  ; an empty InprocServer32 for this CLSID disables the Windows 11 compact context menu (per user)
  WriteRegStr HKCU "Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\InprocServer32" "" ""
  WriteRegDWORD HKCU "Software\${APPNAME}" "ClassicMenu" 1
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} $(DESC_MAIN)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(DESC_DESK)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecOpenWith} $(DESC_OPEN)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecClassic} $(DESC_CLASSIC)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APPNAME}.lnk"
  Delete "$DESKTOP\${APPNAME}.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\Classes\Applications\${EXE}"
  DeleteRegKey HKCU "Software\Classes\${PROGID}"
  DeleteRegValue HKCU "Software\Classes\.inp\OpenWithProgids" "${PROGID}"
  DeleteRegKey HKCU "Software\Classes\SystemFileAssociations\.inp\shell\InpLite"
  DeleteRegKey HKCU "Software\Classes\.sta\OpenWithList\${EXE}"
  ReadRegDWORD $0 HKCU "Software\${APPNAME}" "ClassicMenu"
  ${If} $0 == 1
    DeleteRegKey HKCU "Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}"
  ${EndIf}
  DeleteRegKey HKCU "Software\${APPNAME}"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Function .onInit
  ; keep the full right-click menu option if it was chosen before (also under the name Abapad++)
  ReadRegDWORD $0 HKCU "Software\Abapad++" "ClassicMenu"
  ReadRegDWORD $2 HKCU "Software\${APPNAME}" "ClassicMenu"
  ${If} $0 == 1
  ${OrIf} $2 == 1
    !insertmacro SelectSection ${SecClassic}
  ${EndIf}
FunctionEnd
