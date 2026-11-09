{
  # Slicenv для Nix / NixOS. НЕ ПЕРЕВІРЕНО: Nix на машині розробника не встановлено,
  # перевірено лише синтаксис. Подробиці й відомі ризики — packaging/README.md, «Nix».
  #
  #   nix build            → пакет у FHS-оточенні (рушії-AppImage, плагін Bambu працюють)
  #   nix build .#unwrapped → «чисті» бінарники без FHS
  #   nix run              → запуск
  #   nix develop          → оточення розробника (cargo, clippy, системні бібліотеки)
  description = "Slicenv — 3D printing slicer front-end (OrcaSlicer / Bambu Studio engines)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version =
        (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      appId = "app.borys.slicenv";

      # Бібліотеки, які GPUI/wgpu відкривають через dlopen (немає в DT_NEEDED).
      runtimeLibs = pkgs: with pkgs; [
        wayland
        libxkbcommon
        libxcb
        vulkan-loader
        libGL
        fontconfig
        freetype
      ];

      unwrapped = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "slicenv-unwrapped";
        inherit version;
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let base = baseNameOf path; in
            # result, result-unwrapped… — симлінки nix build; інакше кожен з них змінює
            # хеш джерел і змушує перезбирати все з нуля.
            !(builtins.elem base [ "target" "dist" ".git" ]
              || pkgs.lib.hasPrefix "result" base);
        };
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "-p" "app" "-p" "mcp" ];
        # Тести workspace вимагають GPU/мережі/зразків — у пісочниці Nix не запускаємо.
        doCheck = false;

        nativeBuildInputs = with pkgs; [ pkg-config cmake clang librsvg python3 ];
        buildInputs = (runtimeLibs pkgs) ++ (with pkgs; [ openssl zstd manifold ]);

        # manifold-csg-sys інакше клонує manifold3d з GitHub у build.rs — у пісочниці Nix
        # мережі немає. Потрібна manifold з C-біндінгами (manifoldc) версії 3.5.x.
        MANIFOLD_CSG_LIB_DIR = "${pkgs.manifold}/lib";
        MANIFOLD_CSG_LIB_KIND = "dylib";

        postInstall = ''
          a=crates/app/assets/app
          sed 's|@EXEC@|slicenv|' $a/${appId}.desktop > ${appId}.desktop
          install -Dm644 ${appId}.desktop $out/share/applications/${appId}.desktop
          install -Dm644 $a/${appId}.metainfo.xml $out/share/metainfo/${appId}.metainfo.xml
          install -Dm644 $a/slicenv.svg $out/share/icons/hicolor/scalable/apps/${appId}.svg
          for s in 16 24 32 48 64 128 256 512; do
            mkdir -p $out/share/icons/hicolor/''${s}x$s/apps
            rsvg-convert -w $s -h $s $a/slicenv.svg -o $out/share/icons/hicolor/''${s}x$s/apps/${appId}.png
          done
          install -Dm644 -t $out/share/doc/slicenv LICENSE THIRD-PARTY-NOTICES.md packaging/THIRD-PARTY-RUST-CRATES.md
        '';

        # dlopen-бібліотеки: дописуємо в RUNPATH, щоб GPUI знайшов Wayland/Vulkan без
        # LD_LIBRARY_PATH (який успадкували б рушії).
        postFixup = ''
          for b in $out/bin/slicenv $out/bin/slicenv-mcp; do
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} $b
          done
        '';

        meta = {
          description = "3D printing slicer front-end for OrcaSlicer / Bambu Studio engines";
          license = pkgs.lib.licenses.agpl3Plus;
          platforms = systems;
          mainProgram = "slicenv";
        };
      };

      # Рушії нарізки — звичайні Linux-AppImage (розпаковуються в ~/.local/share/slicenv),
      # а плагін Bambu — закрита .so, зібрана під FHS-дистрибутиви. Обом потрібні
      # /lib64/ld-linux-x86-64.so.2 і бібліотеки за стандартними шляхами, тому основний
      # пакет — buildFHSEnv. Альтернатива на NixOS: programs.nix-ld.enable = true з
      # programs.nix-ld.libraries = <targetPkgs нижче> і пакет .#unwrapped.
      fhs = pkgs: pkgs.buildFHSEnv {
        name = "slicenv";
        targetPkgs = p: [ (unwrapped p) ] ++ (runtimeLibs p) ++ (with p; [
          # менеджер рушіїв і завантаження
          python3 curl gnutar squashfsTools xdg-utils cacert
          # залежності рушіїв OrcaSlicer / Bambu Studio CLI та плагіна Bambu
          # buildFHSEnv кладе в /usr/lib лише перелічені пакети, не їхні залежності: GTK
          # без pango/cairo/atk/harfbuzz/gdk-pixbuf рушій не запуститься.
          glib gtk3 pango cairo atk harfbuzz gdk-pixbuf
          webkitgtk_4_1 libsoup_3 openssl zlib dbus libsecret
          gst_all_1.gstreamer gst_all_1.gst-plugins-base
          # OrcaSlicer 2.5 (AppImage Ubuntu 24.04) лінкується з libavcodec.so.61 (FFmpeg 7)
          # і libmspack; його AppRun шукає libOpenGL.so.0 (libglvnd = libGL).
          ffmpeg_7 libmspack
          mesa libdrm expat
          libx11 libxext libxrender libxrandr libxi libxcursor
          libsm libice
          stdenv.cc.cc.lib
        ]);
        runScript = "slicenv";
        extraInstallCommands = ''
          mkdir -p $out/share
          ln -s ${unwrapped pkgs}/share/applications $out/share/applications
          ln -s ${unwrapped pkgs}/share/icons $out/share/icons
          ln -s ${unwrapped pkgs}/share/metainfo $out/share/metainfo
        '';
        meta = (unwrapped pkgs).meta;
      };
    in
    {
      packages = forAll (pkgs: {
        unwrapped = unwrapped pkgs;
        slicenv = fhs pkgs;
        default = fhs pkgs;
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${fhs pkgs}/bin/slicenv";
          meta.description = "Slicenv GUI (FHS-оточення)";
        };
        mcp = {
          type = "app";
          program = "${unwrapped pkgs}/bin/slicenv-mcp";
          meta.description = "Slicenv MCP-сервер (stdio)";
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (unwrapped pkgs) ];
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ast-grep ripgrep ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
          # У devShell manifold3d можна збирати й самим build.rs (мережа є):
          # unset MANIFOLD_CSG_LIB_DIR MANIFOLD_CSG_LIB_KIND
        };
      });
    };
}
