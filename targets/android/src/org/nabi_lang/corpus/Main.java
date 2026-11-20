package org.nabi_lang.corpus;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

// Only there to make the package an app; the instrumentation does the work.
public final class Main extends Activity {
    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        TextView t = new TextView(this);
        t.setText("Nabi corpus: run org.nabi_lang.corpus.tests/.Corpus");
        setContentView(t);
    }
}
