PORTNAME=	freebsd-timeline
DISTVERSION=	0.1.0
DISTVERSIONPREFIX=	v
CATEGORIES=	sysutils

MAINTAINER=	you@example.org
COMMENT=	ZFS snapshots, boot environments and backups
WWW=		https://github.com/Dishwasher-DEV/freebsd-timeline

LICENSE=	BSD2CLAUSE
LICENSE_FILE=	${WRKSRC}/LICENSE

USES=		cargo
USE_GITHUB=	yes
GH_ACCOUNT=	Dishwasher-DEV
GH_PROJECT=	freebsd-timeline
USE_RC_SUBR=	freebsd-timelined

CARGO_INSTALL_PATH=	crates/freebsd-timeline-cli crates/freebsd-timelined

OPTIONS_DEFINE=		GUI
OPTIONS_DEFAULT=	GUI
OPTIONS_SUB=		yes
GUI_DESC=		GTK4 / libadwaita desktop app
GUI_USES=		gnome
GUI_USE=		GNOME=gtk40,libadwaita
GUI_VARS=		CARGO_INSTALL_PATH+=crates/freebsd-timeline-gtk

post-install:
	${MV} ${STAGEDIR}${PREFIX}/bin/freebsd-timelined ${STAGEDIR}${PREFIX}/sbin/freebsd-timelined
	${INSTALL_DATA} ${WRKSRC}/examples/freebsd-timeline.toml \
		${STAGEDIR}${PREFIX}/etc/freebsd-timeline.toml.sample

post-install-GUI-on:
	${MKDIR} ${STAGEDIR}${PREFIX}/share/applications
	${INSTALL_DATA} ${WRKSRC}/data/io.github.Dishwasher_DEV.FreebsdTimeline.desktop \
		${STAGEDIR}${PREFIX}/share/applications/

# Crate list: run `make cargo-crates > Makefile.crates` after `make extract`.
.include "${.CURDIR}/Makefile.crates"
.include <bsd.port.mk>
