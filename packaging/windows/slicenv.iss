; Inno Setup 6: інсталятор Slicenv для Windows (без підпису). Збирає tools/package-windows.ps1:
;   iscc /DAppVersion=0.1.0 /DSourceDir=dist\slicenv-0.1.0-x86_64-windows /DOutputDir=dist ^
;        /DOutputBase=slicenv-0.1.0-x86_64-windows-setup packaging\windows\slicenv.iss
; Встановлення для поточного користувача без прав адміністратора (PrivilegesRequired=lowest);
; у діалозі можна вибрати «для всіх» (Program Files).
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #error SourceDir не задано
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef OutputBase
  #define OutputBase "slicenv-setup"
#endif

[Setup]
; Стабільний AppId: за ним Windows знаходить попередню установку при оновленні.
AppId={{6F0E1B8C-3D7A-4C52-9B1E-5A2F6C9D7E41}
AppName=Slicenv
AppVersion={#AppVersion}
AppPublisher=Borys Kharchenko
AppPublisherURL=https://github.com/arximus88/slicenv
DefaultDirName={autopf}\Slicenv
DefaultGroupName=Slicenv
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#SourceDir}\LICENSE.txt
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\slicenv.exe
; Відносно теки цього скрипту. Той самий .ico вбудовано в slicenv.exe (crates/app/build.rs).
SetupIconFile=..\..\crates\app\assets\app\slicenv.ico
; Підпис: SignTool=... — див. packaging/README.md, розділ «Підпис».

[Languages]
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
; AppUserModelID — той самий, що ставить процес (APP_ID у crates/app/src/main.rs): інакше
; закріплений ярлик і відкрите вікно — дві окремі кнопки на панелі завдань.
Name: "{group}\Slicenv"; Filename: "{app}\slicenv.exe"; AppUserModelID: "app.borys.slicenv"
Name: "{autodesktop}\Slicenv"; Filename: "{app}\slicenv.exe"; AppUserModelID: "app.borys.slicenv"; Tasks: desktopicon

[Run]
Filename: "{app}\slicenv.exe"; Description: "{cm:LaunchProgram,Slicenv}"; Flags: nowait postinstall skipifsilent
