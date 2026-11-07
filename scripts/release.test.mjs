// Tests for scripts/release.mjs (ADR-0022, S22, U31): the release workflow's guard, its pushes by
// architecture, the multi-arch indexes from their digests, and the draft's body. No test runs git,
// gh or docker: a stand-in answers each call as scripted and records it.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";

import { installerName, workspaceVersion } from "./app-installer.mjs";
import {
  main,
  notes,
  parseTag,
  installerName as releaseInstallerName,
  workspaceVersion as releaseWorkspaceVersion,
} from "./release.mjs";

const SHA = "0123456789abcdef0123456789abcdef01234567";
const OTHER_SHA = "89abcdef0123456789abcdef0123456789abcdef";
const CREATED = "2026-11-07T12:34:56Z";
const DIGEST = (n) => `sha256:${String(n).repeat(64).slice(0, 64)}`;
const RELEASE_ENV = {
  AFKFLEET_IMAGE_VERSION: "0.2.0",
  AFKFLEET_IMAGE_REVISION: SHA,
  AFKFLEET_IMAGE_CREATED: CREATED,
};

const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** A folder of the test's own, with a Cargo.toml at `version` and an empty outputs file. */
function folder(version = "0.2.0") {
  const dir = mkdtempSync(path.join(tmpdir(), "afkfleet-release-"));
  made.push(dir);
  writeFileSync(path.join(dir, "Cargo.toml"), `[workspace.package]\nversion = "${version}"\n`);
  writeFileSync(path.join(dir, "outputs"), "");
  return dir;
}

/** A sink for stdout or stderr. */
function sink() {
  const parts = [];
  return { write: (text) => parts.push(text), text: () => parts.join("") };
}

/**
 * main() in `dir` with `env`. Each call is answered by the first of `answers` whose `when` is its
 * whole command line or a prefix of it ending at a space, and, when the answer says `anonymous`,
 * whose environment sets DOCKER_CONFIG (or doesn't); by default it succeeds with no output.
 */
function run(dir, args, answers = [], env = {}) {
  const calls = [];
  const out = sink();
  const err = sink();
  const code = main(args, {
    root: dir,
    env: {
      GITHUB_OUTPUT: path.join(dir, "outputs"),
      GITHUB_REPOSITORY_OWNER: "Owner",
      GITHUB_SHA: SHA,
      ...env,
    },
    out,
    err,
    now: () => new Date("2026-11-07T12:34:56.789Z"),
    run: (command, commandArgs, options = {}) => {
      const line = [command, ...commandArgs].join(" ");
      const anonymous = options.env?.DOCKER_CONFIG !== undefined;
      calls.push({ line, anonymous });
      const found =
        answers.find(
          (answer) =>
            (line === answer.when || line.startsWith(`${answer.when} `)) &&
            (answer.anonymous === undefined || answer.anonymous === anonymous),
        ) ?? {};
      return { status: found.status ?? 0, stdout: found.stdout ?? "", stderr: found.stderr ?? "" };
    },
  });
  return {
    code,
    out: out.text(),
    err: err.text(),
    calls: calls.map((call) => call.line),
    anonymous: calls.filter((call) => call.anonymous).map((call) => call.line),
    outputs: readFileSync(path.join(dir, "outputs"), "utf8"),
  };
}

describe("release: the tag", () => {
  it("reads vX.Y.Z tags, and nothing else", () => {
    assert.equal(parseTag("refs/tags/v0.2.0"), "0.2.0");
    assert.equal(parseTag("refs/tags/v10.0.31"), "10.0.31");
    for (const tag of [
      "refs/tags/0.2.0",
      "refs/tags/v0.2",
      "refs/tags/v0.2.0-rc.1",
      "refs/tags/v01.2.0",
      "refs/heads/v0.2.0",
      "v0.2.0",
      "refs/tags/v0.2.0\n",
    ]) {
      assert.equal(parseTag(tag), null, JSON.stringify(tag));
    }
  });
});

describe("release guard", () => {
  const RUN_LIST = `gh run list --workflow ci.yml --branch main --commit ${SHA} --event push --status success --json databaseId,headSha,conclusion --limit 1`;
  const green = [
    { when: "git rev-parse", stdout: `${SHA}\n` },
    {
      when: "gh run list",
      stdout: `[{"databaseId":42,"headSha":"${SHA}","conclusion":"success"}]\n`,
    },
  ];

  it("passes a tag equal to the version, on main, with CI green on it; outputs what the jobs need", () => {
    const result = run(folder("0.2.0"), ["guard", "refs/tags/v0.2.0"], green);

    assert.equal(result.code, 0, result.err);
    assert.deepEqual(result.calls, [
      "git rev-parse --verify refs/tags/v0.2.0^{commit}",
      `git merge-base --is-ancestor ${SHA} refs/remotes/origin/main`,
      RUN_LIST,
    ]);
    assert.equal(result.outputs, `version=0.2.0\nrevision=${SHA}\ncreated=${CREATED}\n`);
  });

  it("refuses a malformed tag and another version before asking git anything", () => {
    const malformed = run(folder(), ["guard", "refs/tags/v0.2"], green);
    const other = run(folder("0.3.0"), ["guard", "refs/tags/v0.2.0"], green);

    assert.match(malformed.err, /vX\.Y\.Z/);
    assert.match(other.err, /0\.3\.0/);
    for (const result of [malformed, other]) {
      assert.equal(result.code, 1);
      assert.equal(result.outputs, "");
      assert.deepEqual(result.calls, []);
    }
  });

  it("refuses a tag on another commit than the run's, or off main, before asking for CI", () => {
    const moved = run(folder(), ["guard", "refs/tags/v0.2.0"], green, { GITHUB_SHA: OTHER_SHA });
    const offMain = run(
      folder(),
      ["guard", "refs/tags/v0.2.0"],
      [{ when: "git merge-base", status: 1 }, ...green],
    );

    assert.match(moved.err, /GITHUB_SHA/);
    assert.match(offMain.err, /main/);
    for (const result of [moved, offMain]) {
      assert.equal(result.code, 1);
      assert.equal(result.outputs, "");
      assert.ok(!result.calls.some((call) => call.startsWith("gh ")));
    }
  });

  it("refuses unless CI's own run on that commit succeeded (never Stack's: the image jobs run its tests)", () => {
    const answers = (stdout, status = 0) => [
      { when: "git rev-parse", stdout: `${SHA}\n` },
      { when: "gh run list", stdout, status },
    ];
    const results = [
      run(folder(), ["guard", "refs/tags/v0.2.0"], answers("[]\n")),
      run(
        folder(),
        ["guard", "refs/tags/v0.2.0"],
        answers(`[{"databaseId":7,"headSha":"${OTHER_SHA}","conclusion":"success"}]`),
      ),
      run(
        folder(),
        ["guard", "refs/tags/v0.2.0"],
        answers(`[{"databaseId":7,"headSha":"${SHA}","conclusion":"failure"}]`),
      ),
      run(folder(), ["guard", "refs/tags/v0.2.0"], answers("", 1)),
    ];

    for (const [index, result] of results.entries()) {
      assert.equal(result.code, 1, `case ${index}`);
      assert.match(result.err, /CI/, `case ${index}`);
      assert.equal(result.outputs, "", `case ${index}`);
    }
    const green = run(
      folder(),
      ["guard", "refs/tags/v0.2.0"],
      answers(`[{"databaseId":1,"headSha":"${SHA}","conclusion":"success"}]`),
    );
    assert.equal(green.code, 0, green.err);
    assert.ok(!green.calls.some((call) => /stack/i.test(call)));
  });
});

/** Each image's ID, as `docker image inspect --format {{.Id}}` gives it. */
const ID = { "afkfleet-server": DIGEST(7), "afkfleet-agent": DIGEST(8) };

/** The `:dev` images `just images` and `just test-stack` left. */
const DEV = { "afkfleet-server": "afkfleet-server:dev", "afkfleet-agent": "afkfleet-agent:dev" };

/** The images by their IDs, as a load leaves them. */
const BY_ID = { "afkfleet-server": ID["afkfleet-server"], "afkfleet-agent": ID["afkfleet-agent"] };

/**
 * Images of `arch` at `references`, labelled for the release (with `labels` over it), each with
 * its ID.
 */
function images(arch, references, labels = {}) {
  return Object.entries(references).flatMap(([name, reference]) => [
    {
      when: `docker image inspect --format {{.Os}}/{{.Architecture}} ${reference}`,
      stdout: `linux/${arch}\n`,
    },
    {
      when: `docker image inspect --format {{json .Config.Labels}} ${reference}`,
      stdout: `${JSON.stringify({
        "org.opencontainers.image.title": name,
        "org.opencontainers.image.version": "0.2.0",
        "org.opencontainers.image.revision": SHA,
        "org.opencontainers.image.created": CREATED,
        ...labels,
      })}\n`,
    },
    { when: `docker image inspect --format {{.Id}} ${reference}`, stdout: `${ID[name]}\n` },
  ]);
}

/** The IDs file a build job leaves in `dir`. */
function recorded(dir, arch = "arm64", text = undefined) {
  writeFileSync(
    path.join(dir, `images-${arch}.txt`),
    text ?? `afkfleet-server=${ID["afkfleet-server"]}\nafkfleet-agent=${ID["afkfleet-agent"]}\n`,
  );
  return dir;
}

describe("release save", () => {
  it("saves both tested images by their IDs, once checked, and records the IDs", () => {
    const dir = folder();

    const result = run(dir, ["save", "arm64", dir], images("arm64", DEV), RELEASE_ENV);

    assert.equal(result.code, 0, result.err);
    assert.deepEqual(
      result.calls.filter((call) => call.startsWith("docker save")),
      [
        `docker save --output ${path.join(dir, "afkfleet-server.tar")} ${ID["afkfleet-server"]}`,
        `docker save --output ${path.join(dir, "afkfleet-agent.tar")} ${ID["afkfleet-agent"]}`,
      ],
    );
    assert.equal(
      readFileSync(path.join(dir, "images-arm64.txt"), "utf8"),
      `afkfleet-server=${ID["afkfleet-server"]}\nafkfleet-agent=${ID["afkfleet-agent"]}\n`,
    );
  });

  it("refuses an image of another platform, not labelled for this release, or without an ID, saving nothing", () => {
    const cases = [
      [/linux\/amd64/, images("amd64", DEV)],
      [/revision/, images("arm64", DEV, { "org.opencontainers.image.revision": OTHER_SHA })],
      [/version/, images("arm64", DEV, { "org.opencontainers.image.version": "" })],
      [
        /ID/,
        [
          { when: "docker image inspect --format {{.Id}} afkfleet-agent:dev", stdout: "abc\n" },
          ...images("arm64", DEV),
        ],
      ],
    ];

    for (const [message, answers] of cases) {
      const dir = folder();
      const result = run(dir, ["save", "arm64", dir], answers, RELEASE_ENV);

      assert.equal(result.code, 1, `${message}`);
      assert.match(result.err, message);
      assert.ok(!result.calls.some((call) => call.startsWith("docker save")), `${message}`);
      assert.throws(() => readFileSync(path.join(dir, "images-arm64.txt")));
    }
  });

  it("refuses an unknown architecture, a missing folder and a release environment that isn't set", () => {
    assert.equal(run(folder(), ["save", "riscv64", folder()], [], RELEASE_ENV).code, 2);
    assert.equal(run(folder(), ["save", "arm64"], [], RELEASE_ENV).code, 2);
    assert.equal(
      run(folder(), ["save", "arm64", folder()], [], { ...RELEASE_ENV, AFKFLEET_IMAGE_CREATED: "" })
        .code,
      2,
    );
  });
});

describe("release load", () => {
  /** Answers for loading both tarballs from `dir`, each naming `ids[name]`. */
  function loads(dir, ids = ID) {
    return Object.keys(ID).map((name) => ({
      when: `docker load --input ${path.join(dir, `${name}.tar`)}`,
      stdout: `Loaded image ID: ${ids[name]}\n`,
    }));
  }

  it("loads each tarball, and checks it holds the recorded image, of this platform and release", () => {
    const dir = recorded(folder());

    const result = run(
      dir,
      ["load", "arm64", dir],
      [...loads(dir), ...images("arm64", BY_ID)],
      RELEASE_ENV,
    );

    assert.equal(result.code, 0, result.err);
    assert.deepEqual(
      result.calls.filter((call) => call.startsWith("docker load")),
      [
        `docker load --input ${path.join(dir, "afkfleet-server.tar")}`,
        `docker load --input ${path.join(dir, "afkfleet-agent.tar")}`,
      ],
    );
    assert.ok(!result.calls.some((call) => /docker (login|push|tag)/.test(call)));
  });

  it("refuses a tarball that loads another image, or one of another platform or release", () => {
    const cases = [
      [
        /recorded/,
        (dir) => [...loads(dir, { ...ID, "afkfleet-agent": DIGEST(9) }), ...images("arm64", BY_ID)],
      ],
      [/linux\/amd64/, (dir) => [...loads(dir), ...images("amd64", BY_ID)]],
      [
        /revision/,
        (dir) => [
          ...loads(dir),
          ...images("arm64", BY_ID, { "org.opencontainers.image.revision": OTHER_SHA }),
        ],
      ],
    ];

    for (const [message, answers] of cases) {
      const dir = recorded(folder());
      const result = run(dir, ["load", "arm64", dir], answers(dir), RELEASE_ENV);

      assert.equal(result.code, 1, `${message}: ${result.err}`);
      assert.match(result.err, message);
    }
  });

  it("refuses an IDs file that isn't exactly one well-formed ID per image", () => {
    for (const text of [
      `afkfleet-server=${ID["afkfleet-server"]}\n`,
      `afkfleet-server=${ID["afkfleet-server"]}\nafkfleet-agent=sha256:abc\n`,
    ]) {
      const dir = recorded(folder(), "arm64", text);

      const result = run(dir, ["load", "arm64", dir], loads(dir), RELEASE_ENV);

      assert.equal(result.code, 1);
      assert.deepEqual(result.calls, []);
    }
    const missing = folder();
    assert.equal(run(missing, ["load", "arm64", missing], [], RELEASE_ENV).code, 1);
  });
});

describe("release push", () => {
  const notFound = {
    when: "docker buildx imagetools inspect",
    status: 1,
    stderr: "ERROR: ghcr.io/owner/x:0.2.0-arm64: not found\n",
  };
  const present = Object.entries(ID).map(([, id]) => ({
    when: `docker image inspect --format {{.Id}} ${id}`,
    stdout: `${id}\n`,
  }));
  const pushed = [
    {
      when: "docker image inspect --format {{json .RepoDigests}} ghcr.io/owner/afkfleet-server:0.2.0-arm64",
      stdout: `["ghcr.io/other/afkfleet-server@${DIGEST(9)}","ghcr.io/owner/afkfleet-server@${DIGEST(1)}"]\n`,
    },
    {
      when: "docker image inspect --format {{json .RepoDigests}} ghcr.io/owner/afkfleet-agent:0.2.0-arm64",
      stdout: `["ghcr.io/owner/afkfleet-agent@${DIGEST(2)}"]\n`,
    },
  ];

  it("pushes both loaded images by their IDs, once each tag is free, and records their digests", () => {
    const images = recorded(folder());
    const out = folder();

    const result = run(
      images,
      ["push", "arm64", images, out],
      [notFound, ...present, ...pushed],
      RELEASE_ENV,
    );

    assert.equal(result.code, 0, result.err);
    assert.deepEqual(
      result.calls.filter((call) => /^docker (tag|push|buildx)/.test(call)),
      [
        "docker buildx imagetools inspect ghcr.io/owner/afkfleet-server:0.2.0-arm64",
        "docker buildx imagetools inspect ghcr.io/owner/afkfleet-agent:0.2.0-arm64",
        `docker tag ${ID["afkfleet-server"]} ghcr.io/owner/afkfleet-server:0.2.0-arm64`,
        "docker push ghcr.io/owner/afkfleet-server:0.2.0-arm64",
        `docker tag ${ID["afkfleet-agent"]} ghcr.io/owner/afkfleet-agent:0.2.0-arm64`,
        "docker push ghcr.io/owner/afkfleet-agent:0.2.0-arm64",
      ],
    );
    assert.ok(!result.calls.some((call) => call.includes(":dev")), "never the :dev tags");
    assert.equal(
      readFileSync(path.join(out, "digests-arm64.txt"), "utf8"),
      `afkfleet-server=${DIGEST(1)}\nafkfleet-agent=${DIGEST(2)}\n`,
    );
  });

  it("counts a tag the registry won't let it read as absent: pushing it needs more than reading", () => {
    for (const stderr of [
      "ERROR: ghcr.io/owner/afkfleet-server:0.2.0-arm64: not found\n",
      "ERROR: failed to authorize: denied: denied\n",
      "ERROR: manifest unknown\n",
      "ERROR: name unknown: repository name not known to registry\n",
    ]) {
      const images = recorded(folder());
      const result = run(
        images,
        ["push", "arm64", images, folder()],
        [{ when: "docker buildx imagetools inspect", status: 1, stderr }, ...present, ...pushed],
        RELEASE_ENV,
      );

      assert.equal(result.code, 0, `${stderr}: ${result.err}`);
    }
  });

  it("refuses a tag that exists, or one it can't check, pushing nothing", () => {
    const exists = run(
      folder(),
      ["push", "arm64", recorded(folder()), folder()],
      present,
      RELEASE_ENV,
    );
    const unknown = run(
      folder(),
      ["push", "arm64", recorded(folder()), folder()],
      [
        { when: "docker buildx imagetools inspect", status: 1, stderr: "connection reset\n" },
        ...present,
      ],
      RELEASE_ENV,
    );

    assert.match(exists.err, /exists/);
    assert.match(unknown.err, /can't be checked/);
    for (const result of [exists, unknown]) {
      assert.equal(result.code, 1);
      assert.ok(!result.calls.some((call) => call.startsWith("docker push")));
    }
  });

  it("refuses when a recorded image isn't loaded, before the registry", () => {
    const result = run(
      folder(),
      ["push", "arm64", recorded(folder()), folder()],
      [
        {
          when: `docker image inspect --format {{.Id}} ${ID["afkfleet-agent"]}`,
          status: 1,
          stderr: "Error: No such image\n",
        },
        notFound,
        ...present,
      ],
      RELEASE_ENV,
    );

    assert.equal(result.code, 1);
    assert.ok(!result.calls.some((call) => /^docker (buildx|tag|push)/.test(call)));
  });

  it("refuses a pushed image with no digest in its own repository, and writes no digests", () => {
    const out = folder();
    const result = run(
      folder(),
      ["push", "arm64", recorded(folder()), out],
      [
        notFound,
        ...present,
        {
          when: "docker image inspect --format {{json .RepoDigests}}",
          stdout: `["ghcr.io/other/afkfleet-server@${DIGEST(9)}"]\n`,
        },
      ],
      RELEASE_ENV,
    );

    assert.equal(result.code, 1);
    assert.match(result.err, /digest/);
    assert.throws(() => readFileSync(path.join(out, "digests-arm64.txt")));
  });

  it("refuses an unknown architecture, missing folders and a release environment that isn't set", () => {
    assert.equal(run(folder(), ["push", "riscv64", folder(), folder()], [], RELEASE_ENV).code, 2);
    assert.equal(run(folder(), ["push", "arm64", folder()], [], RELEASE_ENV).code, 2);
    assert.equal(
      run(folder(), ["push", "arm64", folder(), folder()], [], {
        ...RELEASE_ENV,
        AFKFLEET_IMAGE_VERSION: "0.2",
      }).code,
      2,
    );
  });
});

describe("release.mjs itself", () => {
  it("imports nothing of the repository's: the push and merge jobs check out this one file", () => {
    const text = readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
    const imports = [...text.matchAll(/^import .* from "([^"]+)";$/gm)].map((match) => match[1]);

    assert.ok(imports.length > 0);
    for (const source of imports) {
      assert.match(source, /^node:/, source);
    }
  });

  it("names the installer and reads the version as app-installer.mjs does", () => {
    for (const version of ["0.2.0", "10.0.31"]) {
      assert.equal(releaseInstallerName(version), installerName(version));
    }
    for (const cargo of [
      '[workspace.package]\nversion = "0.2.0"\n',
      '[package]\nversion = "0.3.0"\n[workspace.package]\nversion = "0.2.0"\n',
      '[workspace.package]\nversion = "0.2"\n',
      "",
    ]) {
      assert.equal(releaseWorkspaceVersion(cargo), workspaceVersion(cargo), cargo);
    }
  });
});

describe("release merge", () => {
  /** The digests the image jobs left, in `dir`. */
  function digests(dir) {
    writeFileSync(
      path.join(dir, "digests-arm64.txt"),
      `afkfleet-server=${DIGEST(1)}\nafkfleet-agent=${DIGEST(2)}\n`,
    );
    writeFileSync(
      path.join(dir, "digests-amd64.txt"),
      `afkfleet-server=${DIGEST(3)}\nafkfleet-agent=${DIGEST(4)}\n`,
    );
    return dir;
  }

  /** A registry that answers as it should, after `overrides`. */
  function registry(overrides = []) {
    const platform = (reference, arch) => ({
      when: `docker buildx imagetools inspect --format {{json .Image}} ghcr.io/owner/${reference}`,
      stdout: JSON.stringify({ architecture: arch, os: "linux" }),
    });
    const manifest = (reference, digest) => ({
      when: `docker buildx imagetools inspect --format {{json .Manifest}} ghcr.io/owner/${reference}`,
      stdout: JSON.stringify({ digest }),
    });
    const index = (name, arm64, amd64) => ({
      when: `docker buildx imagetools inspect --raw ghcr.io/owner/${name}:0.2.0`,
      stdout: JSON.stringify({
        manifests: [
          { digest: arm64, platform: { os: "linux", architecture: "arm64" } },
          { digest: amd64, platform: { os: "linux", architecture: "amd64" } },
        ],
      }),
    });
    const absent = (name) => ({
      when: `docker buildx imagetools inspect ghcr.io/owner/${name}:0.2.0`,
      anonymous: false,
      status: 1,
      stderr: `ERROR: ghcr.io/owner/${name}:0.2.0: not found\n`,
    });
    return [
      ...overrides,
      platform(`afkfleet-server@${DIGEST(1)}`, "arm64"),
      platform(`afkfleet-agent@${DIGEST(2)}`, "arm64"),
      platform(`afkfleet-server@${DIGEST(3)}`, "amd64"),
      platform(`afkfleet-agent@${DIGEST(4)}`, "amd64"),
      manifest("afkfleet-server:0.2.0-arm64", DIGEST(1)),
      manifest("afkfleet-agent:0.2.0-arm64", DIGEST(2)),
      manifest("afkfleet-server:0.2.0-amd64", DIGEST(3)),
      manifest("afkfleet-agent:0.2.0-amd64", DIGEST(4)),
      absent("afkfleet-server"),
      absent("afkfleet-agent"),
      index("afkfleet-server", DIGEST(1), DIGEST(3)),
      index("afkfleet-agent", DIGEST(2), DIGEST(4)),
      manifest("afkfleet-server:0.2.0", DIGEST(5)),
      manifest("afkfleet-agent:0.2.0", DIGEST(6)),
      {
        when: "docker buildx imagetools inspect",
        anonymous: true,
        status: 1,
        stderr: "ERROR: unauthorized\n",
      },
    ];
  }

  it("builds each index from the digests, never the architecture tags, checks it, and outputs its digest", () => {
    const dir = digests(folder());

    const result = run(dir, ["merge", dir], registry(), RELEASE_ENV);

    assert.equal(result.code, 0, result.err);
    const creates = result.calls.filter((call) => call.includes("imagetools create"));
    const annotations = [
      "--annotation index:org.opencontainers.image.version=0.2.0",
      `--annotation index:org.opencontainers.image.revision=${SHA}`,
      `--annotation index:org.opencontainers.image.created=${CREATED}`,
      "--annotation index:org.opencontainers.image.source=https://github.com/noah-gabel/afkfleet",
      "--annotation index:org.opencontainers.image.licenses=GPL-3.0-or-later",
    ];
    assert.deepEqual(creates, [
      [
        "docker buildx imagetools create --tag ghcr.io/owner/afkfleet-server:0.2.0",
        ...annotations,
        `ghcr.io/owner/afkfleet-server@${DIGEST(1)} ghcr.io/owner/afkfleet-server@${DIGEST(3)}`,
      ].join(" "),
      [
        "docker buildx imagetools create --tag ghcr.io/owner/afkfleet-agent:0.2.0",
        ...annotations,
        `ghcr.io/owner/afkfleet-agent@${DIGEST(2)} ghcr.io/owner/afkfleet-agent@${DIGEST(4)}`,
      ].join(" "),
    ]);
    assert.equal(result.outputs, `server_digest=${DIGEST(5)}\nagent_digest=${DIGEST(6)}\n`);
  });

  it("refuses a digest of another platform, a tag that moved, or an index that exists, creating nothing", () => {
    const cases = [
      [
        /linux\/amd64/,
        {
          when: `docker buildx imagetools inspect --format {{json .Image}} ghcr.io/owner/afkfleet-server@${DIGEST(1)}`,
          stdout: JSON.stringify({ architecture: "amd64", os: "linux" }),
        },
      ],
      [
        /one platform/,
        {
          when: `docker buildx imagetools inspect --format {{json .Image}} ghcr.io/owner/afkfleet-agent@${DIGEST(4)}`,
          stdout: JSON.stringify({ "linux/amd64": { architecture: "amd64", os: "linux" } }),
        },
      ],
      [
        /0\.2\.0-arm64/,
        {
          when: "docker buildx imagetools inspect --format {{json .Manifest}} ghcr.io/owner/afkfleet-server:0.2.0-arm64",
          stdout: JSON.stringify({ digest: DIGEST(9) }),
        },
      ],
      [
        /exists/,
        {
          when: "docker buildx imagetools inspect ghcr.io/owner/afkfleet-agent:0.2.0",
          anonymous: false,
          status: 0,
        },
      ],
      [
        /can't be checked/,
        {
          when: "docker buildx imagetools inspect ghcr.io/owner/afkfleet-agent:0.2.0",
          anonymous: false,
          status: 1,
          stderr: "connection reset\n",
        },
      ],
    ];

    for (const [message, override] of cases) {
      const dir = digests(folder());
      const result = run(dir, ["merge", dir], registry([override]), RELEASE_ENV);

      assert.equal(result.code, 1, `${message}: ${result.err}`);
      assert.match(result.err, message);
      assert.equal(result.outputs, "", `${message}`);
      assert.ok(!result.calls.some((call) => call.includes("imagetools create")), `${message}`);
    }
  });

  it("refuses an index that isn't exactly the two digests it was given", () => {
    const cases = [
      { manifests: [{ digest: DIGEST(1), platform: { os: "linux", architecture: "arm64" } }] },
      {
        manifests: [
          { digest: DIGEST(1), platform: { os: "linux", architecture: "arm64" } },
          { digest: DIGEST(3), platform: { os: "linux", architecture: "amd64" } },
          { digest: DIGEST(7), platform: { os: "unknown", architecture: "unknown" } },
        ],
      },
      {
        manifests: [
          { digest: DIGEST(1), platform: { os: "linux", architecture: "arm64" } },
          { digest: DIGEST(8), platform: { os: "linux", architecture: "amd64" } },
        ],
      },
      {
        manifests: [
          { digest: DIGEST(3), platform: { os: "linux", architecture: "arm64" } },
          { digest: DIGEST(1), platform: { os: "linux", architecture: "amd64" } },
        ],
      },
    ];

    for (const [index, raw] of cases.entries()) {
      const dir = digests(folder());
      const result = run(
        dir,
        ["merge", dir],
        registry([
          {
            when: "docker buildx imagetools inspect --raw ghcr.io/owner/afkfleet-server:0.2.0",
            stdout: JSON.stringify(raw),
          },
        ]),
        RELEASE_ENV,
      );

      assert.equal(result.code, 1, `case ${index}: ${result.err}`);
      assert.match(result.err, /afkfleet-server:0\.2\.0/, `case ${index}`);
      assert.equal(result.outputs, "", `case ${index}`);
    }
  });

  it("refuses digests files that aren't exactly one well-formed digest per image", () => {
    const bad = [
      `afkfleet-server=${DIGEST(1)}\n`,
      `afkfleet-server=${DIGEST(1)}\nafkfleet-agent=sha256:abc\n`,
      `afkfleet-server=${DIGEST(1)}\nafkfleet-agent=${DIGEST(2)}\nafkfleet-other=${DIGEST(3)}\n`,
      `afkfleet-server=${DIGEST(1)}\nafkfleet-server=${DIGEST(2)}\n`,
    ];

    for (const [index, text] of bad.entries()) {
      const dir = digests(folder());
      writeFileSync(path.join(dir, "digests-arm64.txt"), text);

      const result = run(dir, ["merge", dir], registry(), RELEASE_ENV);

      assert.equal(result.code, 1, `case ${index}`);
      assert.deepEqual(result.calls, [], `case ${index}`);
    }
    const missing = folder();
    assert.equal(run(missing, ["merge", missing], registry(), RELEASE_ENV).code, 1);
  });

  it("checks an anonymous pull of each index, and warns while one still needs a login", () => {
    const dir = digests(folder());
    const privately = run(dir, ["merge", dir], registry(), RELEASE_ENV);
    const publicDir = digests(folder());
    const publicly = run(
      publicDir,
      ["merge", publicDir],
      registry([
        {
          when: "docker buildx imagetools inspect ghcr.io/owner/afkfleet-server:0.2.0",
          anonymous: true,
        },
        {
          when: "docker buildx imagetools inspect ghcr.io/owner/afkfleet-agent:0.2.0",
          anonymous: true,
        },
      ]),
      RELEASE_ENV,
    );

    assert.deepEqual(privately.anonymous, [
      "docker buildx imagetools inspect ghcr.io/owner/afkfleet-server:0.2.0",
      "docker buildx imagetools inspect ghcr.io/owner/afkfleet-agent:0.2.0",
    ]);
    assert.equal(privately.code, 0, privately.err);
    assert.match(privately.out, /^::warning::.*afkfleet-server.*Public/m);
    assert.equal(publicly.code, 0, publicly.err);
    assert.doesNotMatch(publicly.out, /::warning::/);
  });
});

describe("release notes", () => {
  it("are the approved body, with the images by digest", () => {
    assert.equal(
      notes({ version: "0.2.0", owner: "owner", server: DIGEST(5), agent: DIGEST(6) }),
      [
        "afkfleet 0.2.0",
        "",
        "Desktop app for Windows: afkfleet_0.2.0_x64-setup.exe",
        "How to install and use it: https://github.com/noah-gabel/afkfleet/blob/main/docs/using-the-app.md",
        "Server images:",
        `  ghcr.io/owner/afkfleet-server:0.2.0@${DIGEST(5)}`,
        `  ghcr.io/owner/afkfleet-agent:0.2.0@${DIGEST(6)}`,
        "SHA-256 of every file: SHA256SUMS",
        "",
      ].join("\n"),
    );
  });

  it("print through the command line, and refuse a malformed digest", () => {
    const result = run(folder(), ["notes", "0.2.0", DIGEST(5), DIGEST(6)]);
    const malformed = run(folder(), ["notes", "0.2.0", DIGEST(5), "sha256:xyz"]);

    assert.equal(result.code, 0, result.err);
    assert.equal(
      result.out,
      notes({ version: "0.2.0", owner: "owner", server: DIGEST(5), agent: DIGEST(6) }),
    );
    assert.equal(malformed.code, 2);
    assert.equal(malformed.out, "");
  });
});

describe("release: usage", () => {
  it("refuses an unknown command and a missing outputs file", () => {
    assert.equal(run(folder(), []).code, 2);
    assert.equal(run(folder(), ["publish"]).code, 2);
    assert.equal(run(folder(), ["guard", "refs/tags/v0.2.0"], [], { GITHUB_OUTPUT: "" }).code, 2);
  });
});
