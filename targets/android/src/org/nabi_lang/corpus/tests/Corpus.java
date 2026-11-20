package org.nabi_lang.corpus.tests;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONObject;

/*
 * Runs every staged corpus case (assets/cases/<name>.json, the program
 * lib<name>.so in the app's nativeLibraryDir) and checks it as the
 * harness's check_run does: exit code, stderr lines against the patterns,
 * stdout. assets/skip.txt names the cases a phone cannot run, with why.
 *
 * Reports each case in `am instrument -r` status form (class
 * org.nabi_lang.corpus.Cases, test <name>) and writes JUnit XML to the
 * app's external files dir (junit.xml).
 *
 * Arguments (`am instrument -e <key> <value>`):
 *   filter  a,b  only cases whose name contains one of them
 *   jobs    n    cases run at once (default: the cores)
 *   timeout s    cap on a run's limit (default 120)
 */
public final class Corpus extends Instrumentation {
    private static final String CLASS = "org.nabi_lang.corpus.Cases";
    private static final String ID = "AndroidJUnitRunner";

    private Bundle args;

    private static final class Result {
        String name;
        String failure; // null: passed
        String skip; // non-null: skipped, the reason
        double secs;
    }

    @Override
    public void onCreate(Bundle arguments) {
        args = arguments == null ? new Bundle() : arguments;
        start();
    }

    @Override
    public void onStart() {
        Bundle end = new Bundle();
        try {
            end.putString("stream", runAll() + "\n");
            finish(Activity.RESULT_OK, end);
        } catch (Throwable t) {
            end.putString("stream", "corpus: " + stack(t) + "\n");
            finish(Activity.RESULT_CANCELED, end);
        }
    }

    private String runAll() throws Exception {
        Context own = getContext();
        Context app = getTargetContext();
        File libs = new File(app.getApplicationInfo().nativeLibraryDir);
        File work = new File(app.getCacheDir(), "corpus");
        deleteTree(work);
        work.mkdirs();

        List<String> names = new ArrayList<>();
        String[] files = own.getAssets().list("cases");
        Arrays.sort(files);
        String[] parts = args.getString("filter", "").split(",");
        for (String f : files) {
            if (!f.endsWith(".json")) {
                continue;
            }
            String name = f.substring(0, f.length() - 5);
            if (selected(name, parts)) {
                names.add(name);
            }
        }
        Map<String, String> skips = readSkips(own);
        int jobs = Integer.parseInt(args.getString("jobs",
                String.valueOf(Runtime.getRuntime().availableProcessors())));
        long cap = Long.parseLong(args.getString("timeout", "120"));

        ExecutorService pool = Executors.newFixedThreadPool(Math.max(1, jobs));
        List<Future<Result>> pending = new ArrayList<>();
        for (String name : names) {
            pending.add(pool.submit(() -> {
                Result r = new Result();
                r.name = name;
                long t0 = System.nanoTime();
                r.skip = skips.get(name);
                if (r.skip == null) {
                    try {
                        JSONObject c = new JSONObject(asset(own, "cases/" + name + ".json"));
                        r.failure = runCase(c, new File(libs, "lib" + name + ".so"),
                                new File(work, name), cap);
                    } catch (Throwable t) {
                        r.failure = "runner: " + stack(t);
                    }
                }
                r.secs = (System.nanoTime() - t0) / 1e9;
                return r;
            }));
        }

        List<Result> results = new ArrayList<>();
        int ran = 0;
        int failed = 0;
        int i = 0;
        for (Future<Result> f : pending) {
            Result r = f.get();
            results.add(r);
            i++;
            Bundle b = new Bundle();
            b.putString("id", ID);
            b.putString("class", CLASS);
            b.putString("test", r.name);
            b.putInt("numtests", names.size());
            b.putInt("current", i);
            sendStatus(1, b);
            if (r.skip != null) {
                b.putString("stream", "skipped " + r.name + ": " + r.skip + "\n");
                sendStatus(-3, b);
            } else if (r.failure != null) {
                ran++;
                failed++;
                b.putString("stack", r.failure);
                b.putString("stream", "FAIL  " + r.name + ": " + r.failure + "\n");
                sendStatus(-2, b);
            } else {
                ran++;
                sendStatus(0, b);
            }
        }
        pool.shutdownNow();
        deleteTree(work);

        File out = app.getExternalFilesDir(null);
        out.mkdirs();
        writeJunit(new File(out, "junit.xml"), results);
        return ran + " of " + names.size() + " cases ran, " + failed + " failed";
    }

    private static boolean selected(String name, String[] parts) {
        boolean any = false;
        for (String p : parts) {
            p = p.trim();
            if (p.isEmpty()) {
                continue;
            }
            any = true;
            if (name.contains(p)) {
                return true;
            }
        }
        return !any;
    }

    // `name reason...` per line; `#` starts a comment line.
    private static Map<String, String> readSkips(Context own) throws IOException {
        Map<String, String> m = new HashMap<>();
        String text;
        try {
            text = asset(own, "skip.txt");
        } catch (IOException e) {
            return m;
        }
        for (String line : text.split("\n")) {
            line = line.trim();
            if (line.isEmpty() || line.startsWith("#")) {
                continue;
            }
            int sp = line.indexOf(' ');
            m.put(sp < 0 ? line : line.substring(0, sp),
                    sp < 0 ? "listed in skip.txt" : line.substring(sp + 1).trim());
        }
        return m;
    }

    // null when every run of the case matches; else what differed.
    private static String runCase(JSONObject c, File exe, File dir, long cap) throws Exception {
        JSONArray runs = c.getJSONArray("runs");
        String[] labels = {"", "gc-stress: ", "gc-stress concurrent: "};
        for (int i = 0; i < runs.length(); i++) {
            String err = runOnce(c, exe, dir, runs.getJSONObject(i), cap);
            if (err != null) {
                return (i < labels.length ? labels[i] : "run " + i + ": ") + err;
            }
        }
        return null;
    }

    private static String runOnce(JSONObject c, File exe, File dir, JSONObject env, long cap)
            throws Exception {
        deleteTree(dir);
        File tmp = new File(dir, "t");
        File cwd = new File(dir, "w");
        tmp.mkdirs();
        cwd.mkdirs();
        File out = new File(dir, "stdout");
        File err = new File(dir, "stderr");
        ProcessBuilder pb = new ProcessBuilder(exe.getPath());
        pb.directory(cwd);
        Map<String, String> e = pb.environment();
        for (String v : new String[] {"TMPDIR", "TMP", "TEMP"}) {
            e.put(v, tmp.getPath());
        }
        for (Iterator<String> it = env.keys(); it.hasNext(); ) {
            String k = it.next();
            e.put(k, env.getString(k));
        }
        pb.redirectInput(ProcessBuilder.Redirect.from(new File("/dev/null")));
        pb.redirectOutput(out);
        pb.redirectError(err);
        long limit = Math.min(c.getLong("timeout"), cap);
        Process p = pb.start();
        if (!p.waitFor(limit, TimeUnit.SECONDS)) {
            p.destroyForcibly();
            p.waitFor();
            return "timed out after " + limit + "s";
        }
        // Death by signal n: 0x80 + n, as a shell reports it.
        int exit = p.exitValue();
        String stderr = read(err);
        String stdout = read(out);
        int want = c.getInt("exit");
        if (exit != want) {
            List<String> lines = lines(stderr);
            int from = Math.max(0, lines.size() - 20);
            return "exit=" + exit + " (expected " + want + ")\n    stderr: "
                    + String.join("\n      ", lines.subList(from, lines.size()));
        }
        String root = c.getString("root");
        String norm = stderr.replace('\\', '/');
        if (!root.isEmpty()) {
            norm = norm.replace(root, "");
        }
        List<String> got = new ArrayList<>();
        for (String l : lines(norm)) {
            got.add(l.trim());
        }
        JSONArray pats = c.getJSONArray("stderr");
        boolean ok = got.size() == pats.length();
        for (int i = 0; ok && i < got.size(); i++) {
            ok = glob(pats.getString(i), got.get(i));
        }
        if (!ok) {
            return "stderr mismatch\n  want: " + pats + "\n  got:  " + new JSONArray(got);
        }
        String wantOut = c.getString("stdout");
        if (!trimEnd(stdout).equals(trimEnd(wantOut))) {
            return "stdout mismatch\n  want: " + JSONObject.quote(wantOut)
                    + "\n  got:  " + JSONObject.quote(stdout);
        }
        return null;
    }

    // Rust's str::lines: split on \n, a trailing \r dropped, no last empty line.
    private static List<String> lines(String s) {
        List<String> out = new ArrayList<>();
        if (s.isEmpty()) {
            return out;
        }
        if (s.endsWith("\n")) {
            s = s.substring(0, s.length() - 1);
        }
        for (String l : s.split("\n", -1)) {
            out.add(l.endsWith("\r") ? l.substring(0, l.length() - 1) : l);
        }
        return out;
    }

    private static String trimEnd(String s) {
        int n = s.length();
        while (n > 0 && Character.isWhitespace(s.charAt(n - 1))) {
            n--;
        }
        return s.substring(0, n);
    }

    // `*` stands for any text, as integration.rs's glob.
    static boolean glob(String pat, String line) {
        String[] parts = pat.split("\\*", -1);
        if (!line.startsWith(parts[0])) {
            return false;
        }
        String rest = line.substring(parts[0].length());
        if (parts.length == 1) {
            return rest.isEmpty();
        }
        for (int i = 1; i < parts.length - 1; i++) {
            int at = rest.indexOf(parts[i]);
            if (at < 0) {
                return false;
            }
            rest = rest.substring(at + parts[i].length());
        }
        return rest.endsWith(parts[parts.length - 1]);
    }

    private static void writeJunit(File f, List<Result> results) throws IOException {
        int failures = 0;
        int skipped = 0;
        double secs = 0;
        for (Result r : results) {
            failures += r.failure != null ? 1 : 0;
            skipped += r.skip != null ? 1 : 0;
            secs += r.secs;
        }
        try (Writer w = new OutputStreamWriter(new FileOutputStream(f), StandardCharsets.UTF_8)) {
            w.write("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
            w.write(String.format("<testsuite name=\"nabi-corpus\" tests=\"%d\" failures=\"%d\""
                    + " errors=\"0\" skipped=\"%d\" time=\"%.3f\">\n",
                    results.size(), failures, skipped, secs));
            for (Result r : results) {
                w.write(String.format("  <testcase classname=\"%s\" name=\"%s\" time=\"%.3f\"",
                        CLASS, xml(r.name), r.secs));
                if (r.skip != null) {
                    w.write(">\n    <skipped message=\"" + xml(r.skip) + "\"/>\n  </testcase>\n");
                } else if (r.failure != null) {
                    String first = r.failure.split("\n", 2)[0];
                    w.write(">\n    <failure message=\"" + xml(first) + "\">" + xml(r.failure)
                            + "</failure>\n  </testcase>\n");
                } else {
                    w.write("/>\n");
                }
            }
            w.write("</testsuite>\n");
        }
    }

    // Text as XML 1.0 allows it: other control characters become U+FFFD.
    private static String xml(String s) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '&': b.append("&amp;"); break;
                case '<': b.append("&lt;"); break;
                case '>': b.append("&gt;"); break;
                case '"': b.append("&quot;"); break;
                default:
                    if (ch < 0x20 && ch != '\n' && ch != '\t' && ch != '\r') {
                        b.append('�');
                    } else {
                        b.append(ch);
                    }
            }
        }
        return b.toString();
    }

    private static String asset(Context c, String path) throws IOException {
        try (InputStream in = c.getAssets().open(path)) {
            return new String(readAll(in), StandardCharsets.UTF_8);
        }
    }

    private static String read(File f) throws IOException {
        try (InputStream in = new FileInputStream(f)) {
            return new String(readAll(in), StandardCharsets.UTF_8);
        }
    }

    private static byte[] readAll(InputStream in) throws IOException {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        byte[] buf = new byte[16384];
        for (int n; (n = in.read(buf)) > 0; ) {
            b.write(buf, 0, n);
        }
        return b.toByteArray();
    }

    // A link a case left is removed, not followed.
    private static void deleteTree(File f) {
        File[] kids = Files.isSymbolicLink(f.toPath()) ? null : f.listFiles();
        if (kids != null) {
            for (File k : kids) {
                deleteTree(k);
            }
        }
        f.delete();
    }

    private static String stack(Throwable t) {
        StringWriter s = new StringWriter();
        t.printStackTrace(new PrintWriter(s));
        return s.toString();
    }
}
