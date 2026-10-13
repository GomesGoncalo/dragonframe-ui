#!/usr/bin/python3
"""Build the frame's screen app (android/ -> android/build/FrameScreen.apk).

    android/build.py

The app is one small activity, so it is built with the Android tools directly rather
than with Gradle. The tools live in .tools/ at the top of this project and are downloaded on
first use: a Java kit (Eclipse Temurin 17) and Google's Android SDK command-line tools,
build tools and Android 10 platform. Fetching the SDK parts accepts Google's Android SDK
licence. Deleting .tools/ removes all of it.

The app is signed with a key made here on first use and kept in .tools/. An app signed
with another key cannot replace it on the frame without being uninstalled first.
"""

import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

APP = Path(__file__).resolve().parent
HERE = APP.parent
TOOLS = HERE / ".tools"
SDK = TOOLS / "sdk"
BUILD_TOOLS = SDK / "build-tools" / "34.0.0"
PLATFORM = SDK / "platforms" / "android-29" / "android.jar"
KEY = TOOLS / "frame-screen.keystore"
KEY_PASSWORD = "frame-screen"  # the key only says "built on this computer"; it guards nothing
OUT = APP / "build"
APK = OUT / "FrameScreen.apk"
PACKAGE = "frame.screen"
JDK_URL = "https://api.adoptium.net/v3/binary/latest/17/ga/mac/aarch64/jdk/hotspot/normal/eclipse"
SDK_TOOLS_URL = "https://dl.google.com/android/repository/commandlinetools-mac-11076708_latest.zip"


def java_home():
    found = sorted(TOOLS.glob("jdk-*/Contents/Home"))
    return found[-1] if found else None


def fetch(url, to):
    print(f"==> downloading {url}")
    with urllib.request.urlopen(url, timeout=600) as reply, open(to, "wb") as out:
        shutil.copyfileobj(reply, out)


def ensure_tools():
    """Download whatever is missing of the Java kit and the Android SDK."""
    TOOLS.mkdir(exist_ok=True)
    if not java_home():
        archive = TOOLS / "jdk.tar.gz"
        fetch(JDK_URL, archive)
        with tarfile.open(archive) as tar:
            tar.extractall(TOOLS)
        archive.unlink()
    env = dict(os.environ, JAVA_HOME=str(java_home()), PATH=f"{java_home() / 'bin'}:{os.environ['PATH']}")
    manager = SDK / "cmdline-tools" / "latest" / "bin" / "sdkmanager"
    if not manager.exists():
        archive = TOOLS / "cmdline.zip"
        fetch(SDK_TOOLS_URL, archive)
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(TOOLS / "unpacked")
        (SDK / "cmdline-tools").mkdir(parents=True, exist_ok=True)
        (TOOLS / "unpacked" / "cmdline-tools").rename(SDK / "cmdline-tools" / "latest")
        (TOOLS / "unpacked").rmdir()
        archive.unlink()
        for tool in (SDK / "cmdline-tools" / "latest" / "bin").iterdir():
            tool.chmod(0o755)  # zipfile drops the executable bit
    if not PLATFORM.exists() or not (BUILD_TOOLS / "aapt2").exists():
        print("==> installing the Android build tools and platform (accepts the Android SDK licence)")
        subprocess.run([str(manager), f"--sdk_root={SDK}", "--licenses"], input="y\n" * 50, text=True, env=env, stdout=subprocess.DEVNULL)
        subprocess.run([str(manager), f"--sdk_root={SDK}", "build-tools;34.0.0", "platforms;android-29"], env=env, check=True, stdout=subprocess.DEVNULL)
    if not KEY.exists():
        subprocess.run(
            ["keytool", "-genkeypair", "-keystore", str(KEY), "-storepass", KEY_PASSWORD, "-keypass", KEY_PASSWORD,
             "-alias", "frame", "-keyalg", "RSA", "-keysize", "2048", "-validity", "36500", "-dname", "CN=Frame screen"],
            env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )  # fmt: skip
    return env


def build():
    """Build and sign the app; returns the path of the APK."""
    env = ensure_tools()
    run = lambda *command: subprocess.run([str(part) for part in command], env=env, check=True)  # noqa: E731
    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "classes").mkdir(parents=True)  # OUT is emptied first, so the APK is always a fresh one
    sources = sorted((APP / "src").rglob("*.java"))
    run("javac", "-Xlint:all", "-Xlint:-options", "-Werror", "--release", "8", "-cp", PLATFORM, "-d", OUT / "classes", *sources)
    run(BUILD_TOOLS / "d8", "--release", "--min-api", "24", "--lib", PLATFORM, "--output", OUT, *sorted((OUT / "classes").rglob("*.class")))
    unaligned = OUT / "unaligned.apk"
    run(BUILD_TOOLS / "aapt2", "link", "-o", unaligned, "--manifest", APP / "AndroidManifest.xml", "-I", PLATFORM)
    with zipfile.ZipFile(unaligned, "a", zipfile.ZIP_DEFLATED) as apk:
        apk.write(OUT / "classes.dex", "classes.dex")
    aligned = OUT / "aligned.apk"
    run(BUILD_TOOLS / "zipalign", "-f", "4", unaligned, aligned)
    run(BUILD_TOOLS / "apksigner", "sign", "--ks", KEY, "--ks-pass", f"pass:{KEY_PASSWORD}", "--out", APK, aligned)
    return APK


if __name__ == "__main__":
    if sys.platform != "darwin":
        sys.exit("The tool downloads here are the macOS ones; adjust JDK_URL and SDK_TOOLS_URL for another system.")
    print(f"Built {build()}")
