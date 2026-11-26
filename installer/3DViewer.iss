; 3DViewer installer: per-user (no admin rights), %LOCALAPPDATA%\Programs\3DViewer by default.
; Build: iscc installer\3DViewer.iss /DAppVersion=0.4.0
; The updater inside the app keeps working because the folder is writable by the user.

#ifndef AppVersion
  #define AppVersion "0.4.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\viewer3d.exe"
#endif

[Setup]
; Keep this AppId: the app reads it to update the installed version after a self-update.
AppId={{6F2B9C41-8E3A-4D57-A1C6-3B7E9D20F5A8}
AppName=3DViewer
AppVersion={#AppVersion}
AppVerName=3DViewer {#AppVersion}
AppPublisher=valuce
AppPublisherURL=https://github.com/valuce/3DViewer
AppSupportURL=https://github.com/valuce/3DViewer/issues
AppUpdatesURL=https://github.com/valuce/3DViewer/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription=3DViewer setup
DefaultDirName={localappdata}\Programs\3DViewer
DefaultGroupName=3DViewer
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\viewer3d.exe
UninstallDisplayName=3DViewer
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=3DViewer-{#AppVersion}-setup
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
ShowLanguageDialog=auto
; App logo in the wizard's top-right corner (100%, 150%, 200% DPI).
WizardSmallImageFile=wizard-small-58.bmp,wizard-small-87.bmp,wizard-small-116.bmp

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
ru.AssocGroup=Ассоциации файлов:
en.AssocGroup=File associations:
ru.AssocTask=Открывать 3D-файлы в 3DViewer (по умолчанию — только для форматов, у которых ещё нет программы; изменить можно в программе: Файл → Ассоциации файлов)
en.AssocTask=Open 3D files with 3DViewer (it becomes the default only for formats with no program yet; change it later in File → File associations)

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "assoc"; Description: "{cm:AssocTask}"; GroupDescription: "{cm:AssocGroup}"
Name: "assoc\gltf"; Description: "glTF (.glb, .gltf)"
Name: "assoc\fbx"; Description: "Autodesk FBX (.fbx)"
Name: "assoc\obj"; Description: "Wavefront OBJ (.obj)"
Name: "assoc\stl"; Description: "STL (.stl)"
Name: "assoc\ply"; Description: "PLY (.ply)"
Name: "assoc\3mf"; Description: "3MF (.3mf)"
Name: "assoc\dae"; Description: "COLLADA (.dae)"
Name: "assoc\3ds"; Description: "3D Studio (.3ds)"
Name: "assoc\off"; Description: "OFF (.off)"
Name: "assoc\blend"; Description: "Blender (.blend)"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "viewer3d.exe"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\3DViewer"; Filename: "{app}\viewer3d.exe"; Comment: "3D model viewer"
Name: "{autodesktop}\3DViewer"; Filename: "{app}\viewer3d.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\viewer3d.exe"; Parameters: "--register glb gltf"; Tasks: assoc\gltf; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register fbx"; Tasks: assoc\fbx; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register obj"; Tasks: assoc\obj; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register stl"; Tasks: assoc\stl; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register ply"; Tasks: assoc\ply; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register 3mf"; Tasks: assoc\3mf; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register dae"; Tasks: assoc\dae; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register 3ds"; Tasks: assoc\3ds; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register off"; Tasks: assoc\off; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Parameters: "--register blend"; Tasks: assoc\blend; Flags: runhidden waituntilterminated
Filename: "{app}\viewer3d.exe"; Description: "{cm:LaunchProgram,3DViewer}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\viewer3d.exe"; Parameters: "--unregister"; Flags: runhidden waituntilterminated; RunOnceId: "Unregister"

[UninstallDelete]
; Leftovers of an interrupted self-update.
Type: files; Name: "{app}\viewer3d.exe.old"
Type: files; Name: "{app}\viewer3d.exe.new"
