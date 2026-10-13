; note setup program. Modern UI 2, Unicode, per-user or per-machine (MultiUser.nsh).
;
; Build (the -D options are required; -WX makes warnings fail the build):
;   makensis -WX -DVERSION=0.1.0 -DEXE=path/to/note.exe -DOUTFILE=path/to/setup.exe installer/note.nsi
; Windows makensis also accepts /D and /WX. Paths are relative to this file's folder, because
; makensis changes into it unless -NOCD is given.
;
; Silent:    setup.exe /S [/CurrentUser | /AllUsers] [/D=<install dir>]
; Uninstall: "<install dir>\uninstall.exe" /S [/CurrentUser | /AllUsers]

Unicode true
SetCompressor /SOLID lzma
ManifestDPIAware true

!ifndef VERSION
  !error "VERSION is not defined. Pass /DVERSION=x.y.z to makensis."
!endif
!ifndef EXE
  !error "EXE is not defined. Pass /DEXE=path/to/note.exe to makensis."
!endif
!ifndef OUTFILE
  !error "OUTFILE is not defined. Pass /DOUTFILE=path/to/setup.exe to makensis."
!endif
!if /FileExists "${EXE}"
!else
  !error "EXE does not exist: ${EXE}"
!endif
!searchparse /noerrors "${VERSION}" "" VMAJOR "." VMINOR "." VPATCH
!ifndef VPATCH
  !error "VERSION must be x.y.z, got ${VERSION}"
!endif
!define VIVERSION "${VMAJOR}.${VMINOR}.${VPATCH}.0"

!define APPNAME "note"
!define APPEXE "note.exe"
!define APPCLASS "note.window"   ; window class registered in src/win/mod.rs
!define PUBLISHER "note"
!define HOMEPAGE "https://github.com/modpotato/note"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\note"
!define PROGID "note.txt"
!define PATH_CU "Environment"
!define PATH_LM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"

; Installer and uninstaller icon (resource ID 1 of the exe is the same icon).
!define MUI_ICON "../assets/icon/note.ico"
!define MUI_UNICON "../assets/icon/note.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APPEXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Run ${APPNAME}"

!define MULTIUSER_EXECUTIONLEVEL Highest
!define MULTIUSER_MUI
!define MULTIUSER_INSTALLMODE_COMMANDLINE
!define MULTIUSER_INSTALLMODE_DEFAULT_CURRENTUSER
!define MULTIUSER_INSTALLMODE_INSTDIR "${APPNAME}"
!define MULTIUSER_USE_PROGRAMFILES64
; An earlier install's InstallLocation is the default directory for upgrades, per mode.
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_KEY "${UNINST_KEY}"
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_VALUENAME "InstallLocation"
; Runs after MultiUser picks the directory for each mode, so a /D= path still wins.
!define MULTIUSER_INSTALLMODE_FUNCTION ApplyCmdLineInstDir
!include MultiUser.nsh
!include FileFunc.nsh
!include StrFunc.nsh
!include WinMessages.nsh
${Using:StrFunc} UnStrRep

Name "${APPNAME} ${VERSION}"
OutFile "${OUTFILE}"
BrandingText " "
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VIVERSION}"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileDescription" "${APPNAME} setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "../LICENSE"
!insertmacro MULTIUSER_PAGE_INSTALLMODE
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------------------
; Helpers
; ---------------------------------------------------------------------------

; Runs ACTION with HKLM for an all-users install and HKCU for a per-user one.
!macro FOR_MODE ACTION
  ${If} $MultiUser.InstallMode == "AllUsers"
    !insertmacro ${ACTION} HKLM
  ${Else}
    !insertmacro ${ACTION} HKCU
  ${EndIf}
!macroend

; Runs ACTION for each extension in the "Open with" list.
!macro FOR_EACH_EXT ACTION ROOT
  !insertmacro ${ACTION} ${ROOT} ".txt"
  !insertmacro ${ACTION} ${ROOT} ".log"
  !insertmacro ${ACTION} ${ROOT} ".md"
  !insertmacro ${ACTION} ${ROOT} ".ini"
  !insertmacro ${ACTION} ${ROOT} ".cfg"
  !insertmacro ${ACTION} ${ROOT} ".conf"
  !insertmacro ${ACTION} ${ROOT} ".json"
  !insertmacro ${ACTION} ${ROOT} ".toml"
  !insertmacro ${ACTION} ${ROOT} ".yaml"
  !insertmacro ${ACTION} ${ROOT} ".yml"
  !insertmacro ${ACTION} ${ROOT} ".xml"
  !insertmacro ${ACTION} ${ROOT} ".csv"
  !insertmacro ${ACTION} ${ROOT} ".rs"
  !insertmacro ${ACTION} ${ROOT} ".c"
  !insertmacro ${ACTION} ${ROOT} ".h"
  !insertmacro ${ACTION} ${ROOT} ".cpp"
  !insertmacro ${ACTION} ${ROOT} ".hpp"
  !insertmacro ${ACTION} ${ROOT} ".cs"
  !insertmacro ${ACTION} ${ROOT} ".py"
  !insertmacro ${ACTION} ${ROOT} ".js"
  !insertmacro ${ACTION} ${ROOT} ".ts"
  !insertmacro ${ACTION} ${ROOT} ".go"
  !insertmacro ${ACTION} ${ROOT} ".lua"
  !insertmacro ${ACTION} ${ROOT} ".sql"
  !insertmacro ${ACTION} ${ROOT} ".sh"
  !insertmacro ${ACTION} ${ROOT} ".ps1"
  !insertmacro ${ACTION} ${ROOT} ".bat"
  !insertmacro ${ACTION} ${ROOT} ".cmd"
  !insertmacro ${ACTION} ${ROOT} ".html"
  !insertmacro ${ACTION} ${ROOT} ".css"
!macroend

; Defines AppIsRunning (pushes 1 or 0) and CheckAppRunning (retry loop) for the setup
; (FN "", LBL "") or the uninstaller (FN "un.", LBL "Un").
!macro DEFINE_RUNNING_CHECK FN LBL
  Function ${FN}AppIsRunning
    ; A window of the running app, or the exe held open by a running process.
    FindWindow $0 "${APPCLASS}"
    ${If} $0 <> 0
      Push 1
      Return
    ${EndIf}
    ${IfNot} ${FileExists} "$INSTDIR\${APPEXE}"
      Push 0
      Return
    ${EndIf}
    ClearErrors
    FileOpen $0 "$INSTDIR\${APPEXE}" a
    ${If} ${Errors}
      Push 1
    ${Else}
      FileClose $0
      Push 0
    ${EndIf}
  FunctionEnd

  Function ${FN}CheckAppRunning
    CheckRetry${LBL}:
    Call ${FN}AppIsRunning
    Pop $0
    ${If} $0 == 0
      Return
    ${EndIf}
    ${IfThen} ${Silent} ${|} Abort "${APPNAME} is running. Close it and run the installer again." ${|}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${APPNAME} is running. Close it, then choose Retry." IDRETRY CheckRetry${LBL}
    Abort
  FunctionEnd
!macroend

!macro REG_UNINSTALL ROOT
  WriteRegStr ${ROOT} "${UNINST_KEY}" "DisplayName" "${APPNAME}"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APPEXE},0"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\" /$MultiUser.InstallMode"
  WriteRegStr ${ROOT} "${UNINST_KEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S /$MultiUser.InstallMode"
  WriteRegDWORD ${ROOT} "${UNINST_KEY}" "EstimatedSize" $0
  WriteRegDWORD ${ROOT} "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD ${ROOT} "${UNINST_KEY}" "NoRepair" 1
  WriteRegStr ${ROOT} "${UNINST_KEY}" "URLInfoAbout" "${HOMEPAGE}"
!macroend

!macro REG_CONTEXT_MENU ROOT
  WriteRegStr ${ROOT} "Software\Classes\*\shell\${APPNAME}" "" "Open with ${APPNAME}"
  WriteRegStr ${ROOT} "Software\Classes\*\shell\${APPNAME}" "Icon" "$INSTDIR\${APPEXE},0"
  WriteRegStr ${ROOT} "Software\Classes\*\shell\${APPNAME}\command" "" "$\"$INSTDIR\${APPEXE}$\" $\"%1$\""
!macroend

!macro REG_OPEN_WITH ROOT
  ; The ProgID is only listed under OpenWithProgids. No default is set, so existing
  ; associations stay as they are.
  WriteRegStr ${ROOT} "Software\Classes\${PROGID}" "" "${APPNAME} text file"
  WriteRegStr ${ROOT} "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\${APPEXE},0"
  WriteRegStr ${ROOT} "Software\Classes\${PROGID}\shell\open\command" "" "$\"$INSTDIR\${APPEXE}$\" $\"%1$\""
  WriteRegStr ${ROOT} "Software\Classes\Applications\${APPEXE}" "FriendlyAppName" "${APPNAME}"
  WriteRegStr ${ROOT} "Software\Classes\Applications\${APPEXE}\shell\open\command" "" "$\"$INSTDIR\${APPEXE}$\" $\"%1$\""
  !insertmacro FOR_EACH_EXT ASSOC_ADD ${ROOT}
!macroend

!macro ASSOC_ADD ROOT EXT
  WriteRegNone ${ROOT} "Software\Classes\${EXT}\OpenWithProgids" "${PROGID}"
  WriteRegStr ${ROOT} "Software\Classes\Applications\${APPEXE}\SupportedTypes" "${EXT}" ""
!macroend

!macro ASSOC_REMOVE ROOT EXT
  DeleteRegValue ${ROOT} "Software\Classes\${EXT}\OpenWithProgids" "${PROGID}"
  DeleteRegKey /ifempty ${ROOT} "Software\Classes\${EXT}\OpenWithProgids"
  DeleteRegKey /ifempty ${ROOT} "Software\Classes\${EXT}"
!macroend

; Adds $INSTDIR to the PATH value at ROOT\KEY, only if it is not there already, and
; records that it did so (PathAdded) so the uninstaller removes only that entry.
; HKEY is the predefined root as a signed 32-bit number, so it sign-extends on 64-bit.
!macro PATH_ADD ROOT KEY HKEY
  ; Measure the stored value before reading it. ReadRegStr returns an empty string for a
  ; value longer than NSIS can hold, and writing after that would erase the whole PATH.
  StrCpy $R9 -1                 ; byte size of the value, -1 when it is missing
  System::Call 'advapi32::RegOpenKeyExW(p ${HKEY}, t "${KEY}", i 0, i 0x20019, *p .R8) i .R7'
  ${If} $R7 = 0
    System::Call 'advapi32::RegQueryValueExW(p $R8, t "Path", p 0, p 0, p 0, *i .R9) i .R7'
    System::Call 'advapi32::RegCloseKey(p $R8)'
  ${EndIf}
  StrCpy $R0 ""
  ${If} $R9 <> -1
    IntOp $R9 $R9 / 2
    IntOp $R9 $R9 - 1           ; characters, without the terminator
  ${EndIf}
  StrLen $R2 $INSTDIR
  IntOp $R3 $R9 + $R2
  IntOp $R3 $R3 + 2             ; room for the separator and the terminator
  ${If} $R3 >= ${NSIS_MAX_STRLEN}
    DetailPrint "Not added to PATH: the current PATH is too long to edit safely. Add $INSTDIR by hand."
  ${Else}
    ${If} $R9 <> -1
      ReadRegStr $R0 ${ROOT} "${KEY}" "Path"
    ${EndIf}
    ${StrStr} $R1 ";$R0;" ";$INSTDIR;"
    ${If} $R1 != ""
      DetailPrint "$INSTDIR is already on PATH."
    ${Else}
      StrCpy $R5 $R0 1 -1
      ${If} $R0 == ""
        StrCpy $R4 "$INSTDIR"
      ${ElseIf} $R5 == ";"
        StrCpy $R4 "$R0$INSTDIR"
      ${Else}
        StrCpy $R4 "$R0;$INSTDIR"
      ${EndIf}
      WriteRegExpandStr ${ROOT} "${KEY}" "Path" $R4
      WriteRegDWORD ${ROOT} "${UNINST_KEY}" "PathAdded" 1
      SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
    ${EndIf}
  ${EndIf}
!macroend

; Removes the $INSTDIR entry from ROOT\KEY, but only when this installer added it.
!macro PATH_REMOVE ROOT KEY
  ReadRegDWORD $R9 ${ROOT} "${UNINST_KEY}" "PathAdded"
  ${If} $R9 == 1
    ClearErrors
    ReadRegStr $R0 ${ROOT} "${KEY}" "Path"
    ${If} ${Errors}
      ; Too long for NSIS to read. Writing back a partial value would damage the PATH.
      DetailPrint "Could not read PATH, so $INSTDIR was not removed from it. Remove it by hand."
    ${Else}
      ; Work on ";entries;" so that the first and last entries match like the others.
      StrCpy $R1 ";$R0;"
      ${UnStrRep} $R1 $R1 ";$INSTDIR;" ";"
      StrCpy $R1 $R1 "" 1
      StrCpy $R1 $R1 -1
      ${If} $R1 != $R0
        ${If} $R1 == ""
          DeleteRegValue ${ROOT} "${KEY}" "Path"
        ${Else}
          WriteRegExpandStr ${ROOT} "${KEY}" "Path" $R1
        ${EndIf}
        SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro UNREG_ALL ROOT
  DeleteRegKey ${ROOT} "Software\Classes\*\shell\${APPNAME}"
  DeleteRegKey ${ROOT} "Software\Classes\${PROGID}"
  DeleteRegKey ${ROOT} "Software\Classes\Applications\${APPEXE}"
  !insertmacro FOR_EACH_EXT ASSOC_REMOVE ${ROOT}
  ; Parents that were empty before are removed too. /ifempty leaves any other data alone.
  DeleteRegKey /ifempty ${ROOT} "Software\Classes\*\shell"
  DeleteRegKey /ifempty ${ROOT} "Software\Classes\*"
  DeleteRegKey /ifempty ${ROOT} "Software\Classes\Applications"
  DeleteRegKey ${ROOT} "${UNINST_KEY}"
!macroend

!insertmacro DEFINE_RUNNING_CHECK "" ""
!insertmacro DEFINE_RUNNING_CHECK "un." "Un"

; ---------------------------------------------------------------------------
; Setup
; ---------------------------------------------------------------------------

Var CmdLineInstDir

Function .onInit
  ; The 64-bit view is needed for the uninstall key of the all-users install.
  SetRegView 64
  ; /D= has already set $INSTDIR. Keep it, because MultiUser overwrites $INSTDIR next.
  StrCpy $CmdLineInstDir $INSTDIR
  !insertmacro MULTIUSER_INIT
FunctionEnd

Function ApplyCmdLineInstDir
  ${If} $CmdLineInstDir != ""
    StrCpy $INSTDIR $CmdLineInstDir
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ; Tell Explorer that file associations changed.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
FunctionEnd

Section "${APPNAME}" SecApp
  SectionIn RO
  Call CheckAppRunning

  SetOutPath "$INSTDIR"
  File "/oname=${APPEXE}" "${EXE}"
  File "/oname=LICENSE" "../LICENSE"
  SetOutPath "$INSTDIR\licenses"
  File "/oname=PaperMono-OFL.txt" "../assets/fonts/OFL.txt"
  SetOutPath "$INSTDIR"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  !insertmacro FOR_MODE REG_UNINSTALL
SectionEnd

Section "Start menu shortcut" SecStartMenu
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${APPEXE}"
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortCut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${APPEXE}"
SectionEnd

Section "Explorer context menu" SecContextMenu
  !insertmacro FOR_MODE REG_CONTEXT_MENU
SectionEnd

Section "Open with list" SecOpenWith
  !insertmacro FOR_MODE REG_OPEN_WITH
SectionEnd

Section /o "Add ${APPNAME} to PATH" SecPath
  ${If} $MultiUser.InstallMode == "AllUsers"
    !insertmacro PATH_ADD HKLM "${PATH_LM}" -2147483646
  ${Else}
    !insertmacro PATH_ADD HKCU "${PATH_CU}" -2147483647
  ${EndIf}
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecApp} "The program, its licenses and the uninstaller. Required."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartMenu} "A Start menu entry for ${APPNAME}."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "A shortcut on the desktop."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecContextMenu} "Adds 'Open with ${APPNAME}' to the right-click menu for every file."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecOpenWith} "Lists ${APPNAME} under 'Open with' for text and code files. Your current default programs are not changed."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPath} "Adds the install folder to PATH, so ${APPNAME} can be run from a terminal."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------------------------------------------------------------------------
; Uninstaller
; ---------------------------------------------------------------------------

Function un.onInit
  SetRegView 64
  !insertmacro MULTIUSER_UNINIT
  ; Without /AllUsers or /CurrentUser the default is per-user. An all-users install has
  ; no per-user key, so fall back to the machine key when only that one exists.
  ${If} $MultiUser.InstallMode == "CurrentUser"
    ReadRegStr $0 HKCU "${UNINST_KEY}" "UninstallString"
    ${If} $0 == ""
      ReadRegStr $0 HKLM "${UNINST_KEY}" "UninstallString"
      ${If} $0 != ""
        Call un.MultiUser.InstallMode.AllUsers
      ${EndIf}
    ${EndIf}
  ${EndIf}
FunctionEnd

Function un.onUninstSuccess
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
FunctionEnd

Section "Uninstall"
  Call un.CheckAppRunning

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"

  ${If} $MultiUser.InstallMode == "AllUsers"
    !insertmacro PATH_REMOVE HKLM "${PATH_LM}"
  ${Else}
    !insertmacro PATH_REMOVE HKCU "${PATH_CU}"
  ${EndIf}
  !insertmacro FOR_MODE UNREG_ALL

  Delete "$INSTDIR\${APPEXE}"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\licenses\PaperMono-OFL.txt"
  RMDir "$INSTDIR\licenses"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ; Settings belong to the user, so they stay unless the user asks for them to go.
  ; A silent uninstall keeps them.
  ${IfNot} ${Silent}
    SetShellVarContext current
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Also delete your settings (%APPDATA%\${APPNAME})?" IDNO +2
    RMDir /r "$APPDATA\${APPNAME}"
  ${EndIf}
SectionEnd
