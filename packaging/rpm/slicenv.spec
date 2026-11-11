# RPM із готового дерева tools/package.sh (stage): rpmbuild -bb --define "stagedir …".
# Збірка з джерел для COPR/Fedora — окремий spec з %cargo_* макросами (див. packaging/README.md).
%global debug_package %{nil}
%global __brp_strip %{nil}
%global __brp_check_rpaths %{nil}
%{!?pkgversion: %global pkgversion 0.1.4}
%global _docdir_fmt %{name}

Name:           slicenv
Version:        %{pkgversion}
Release:        1%{?dist}
Summary:        3D printing slicer front-end (OrcaSlicer / Bambu Studio engines)
License:        AGPL-3.0-or-later
Packager:       Borys Kharchenko <7512615+arximus88@users.noreply.github.com>
ExclusiveArch:  x86_64
# Автоматичні Requires за sonames (libxkbcommon, libxcb…) генерує rpmbuild. Решта — runtime:
# Vulkan-loader і libwayland відкриваються через dlopen, python3 запускає менеджер рушіїв.
# Саме sonames, не назви пакетів: у Fedora це vulkan-loader/libwayland-client, в openSUSE —
# libvulkan1/libwayland-client0, і назви Fedora там не розв’язуються.
Requires:       libvulkan.so.1()(64bit)
Requires:       libwayland-client.so.0()(64bit)
Requires:       python3 >= 3.10
Requires:       curl
Requires:       tar
Requires:       xdg-utils
Recommends:     squashfs-tools
Recommends:     mesa-vulkan-drivers

%description
Slicenv arranges and orients models on build plates, edits print, filament and
printer profiles and drives OrcaSlicer or Bambu Studio command-line engines to
produce G-code, with a fast GPU preview. Includes slicenv-mcp, an MCP server
for AI agents. Slicing engines and the proprietary Bambu network plugin are
not included; they are downloaded on demand, the plugin only with consent.

%prep
cp %{stagedir}/usr/share/doc/slicenv/LICENSE .

%build

%install
mkdir -p %{buildroot}
cp -a %{stagedir}/usr %{buildroot}/
# Ліцензії — через %%license з власного каталогу.
rm -f %{buildroot}%{_docdir}/%{name}/LICENSE

%files
%license LICENSE
%{_bindir}/slicenv
%{_bindir}/slicenv-mcp
%{_datadir}/applications/app.borys.slicenv.desktop
%{_datadir}/metainfo/app.borys.slicenv.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/app.borys.slicenv.*
%doc %{_docdir}/%{name}

%changelog
* Wed Nov 11 2026 Borys Kharchenko <7512615+arximus88@users.noreply.github.com> - 0.1.4-1
- GNOME look (Adwaita palette, header bar, tabs); engine installation stays visible and can be cancelled.

* Wed Nov 11 2026 Borys Kharchenko <7512615+arximus88@users.noreply.github.com> - 0.1.3-1
- OrcaSlicer 2.5 profiles are read from the package cache; clearer no-engine and no-printer states.

* Wed Nov 11 2026 Borys Kharchenko <7512615+arximus88@users.noreply.github.com> - 0.1.2-1
- Engine log lines reporting no substitutions are no longer shown as warnings.

* Wed Nov 11 2026 Borys Kharchenko <7512615+arximus88@users.noreply.github.com> - 0.1.1-1
- Project tabs, floating project bar, printer connections, plugin logging, print routing and interface contrast.

* Fri Nov 06 2026 Borys Kharchenko <7512615+arximus88@users.noreply.github.com> - 0.1.0-1
- Перший пакет.
