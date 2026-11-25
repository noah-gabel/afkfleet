# The Paper harness (Plan.md P16.1, ADR-0049): Paper 26.1.2 with afkfleet's test
# plugin, for the slow tests of inventories and menus. Test-only: built here by
# `just paper-image` as afkfleet-paper:test with harness/paper/ as its whole
# context, never pushed, and nothing that ships holds any of it.
#
# Every input is pinned and checked before it's used:
# - the JDK image by digest: Temurin 25.0.5+8, eclipse-temurin:25-jdk-noble as
#   of 2026-11-12 (docker-library/repo-info);
# - Paper's jar and Mojang's server jar by SHA-256: `ADD --checksum` fails the
#   build on any other bytes. PaperMC's download API publishes the first; the
#   first names the second in its META-INF/download-context;
# - every library Paper's launcher extracts by the SHA-256 the pinned jar lists
#   in its META-INF/libraries.list, which the launcher checks itself.
# Nothing after the two downloads reaches the network: the launcher patches and
# extracts with `--network=none`, and the plugin is compiled by the image's own
# javac against the paper-api jar the launcher extracted. No build tool, no
# repository, no other dependency.
#
# A Minecraft-version bump changes the two jars and their checksums here, and
# the plugin's api-version, with azalea and the vanilla test server's VERSION
# (docs/releasing.md).

FROM eclipse-temurin@sha256:80f67fb3d6f76e2d817d98a4cc2a9cf05f054d4e6adc5e2144bde671d722e63f AS jdk

# Paper's launcher, then the server it patches together offline.
FROM jdk AS server
WORKDIR /paper
ADD --checksum=sha256:1d70b1dab9cf4a6de615209a536f3a45a2186240253c428213ce2188ab95e5f7 \
    https://fill-data.papermc.io/v1/objects/1d70b1dab9cf4a6de615209a536f3a45a2186240253c428213ce2188ab95e5f7/paper-26.1.2-74.jar \
    paper.jar
# Where the launcher looks for Mojang's jar first: found with the right SHA-256,
# it downloads nothing.
ADD --checksum=sha256:cd47e7c38328f64768fd17af8fcd8b22496b40b63d4ffee81e71ae059fedcb42 \
    https://piston-data.mojang.com/v1/objects/97ccd4c0ed3f81bbb7bfacddd1090b0c56f9bc51/server.jar \
    cache/mojang_26.1.2.jar
RUN --network=none java -Dpaperclip.patchonly=true -jar paper.jar

# The test plugin, against exactly the API the server above runs. Every lint is
# an error but `classfile`: paper-api's classes name JetBrains' annotations,
# which aren't among the server's libraries and which nothing needs at run time.
FROM jdk AS plugin
COPY --from=server /paper/libraries /libraries
COPY plugin /plugin
RUN --network=none set -eu; \
    classpath=$(find /libraries -name '*.jar' | sort | tr '\n' ':'); \
    javac --release 25 -Xlint:all,-classfile -Werror -encoding UTF-8 -cp "$classpath" -d /out \
        $(find /plugin/src -name '*.java' | sort); \
    cp /plugin/plugin.yml /out/plugin.yml; \
    jar --create --file /afkfleet-harness.jar --date=2026-11-25T00:00:00Z -C /out .

FROM jdk
COPY --from=server --chown=1000:1000 /paper /paper
COPY --from=plugin --chown=1000:1000 /afkfleet-harness.jar /paper/plugins/afkfleet-harness.jar
COPY --chown=1000:1000 server/ /paper/
COPY --chmod=755 server/rcon /usr/local/bin/rcon
WORKDIR /paper
USER 1000:1000
EXPOSE 25565
HEALTHCHECK --interval=5s --timeout=10s --start-period=3m --retries=20 CMD ["rcon", "list"]
ENTRYPOINT ["sh", "/paper/start.sh"]
