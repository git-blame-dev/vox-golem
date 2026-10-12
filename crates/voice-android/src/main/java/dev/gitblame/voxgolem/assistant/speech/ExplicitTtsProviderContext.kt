package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.ServiceConnection
import android.annotation.TargetApi
import android.os.Build
import java.util.concurrent.Executor

/** Android's TTS constructor may otherwise silently bind a fallback engine. This context
 * is private to one TTS instance and refuses service bindings outside its explicit choice. */
internal open class ExplicitTtsProviderContext(context: Context, private val provider: String) : ContextWrapper(context) {
    override fun getApplicationContext(): Context = this
    protected fun allowed(service: Intent) = (service.component?.packageName ?: service.`package`) == provider
    override fun bindService(service: Intent, connection: ServiceConnection, flags: Int): Boolean =
        allowed(service) && super.bindService(service, connection, flags)
    @TargetApi(29)
    override fun bindService(service: Intent, flags: Int, executor: Executor, connection: ServiceConnection): Boolean =
        allowed(service) && super.bindService(service, flags, executor, connection)
    companion object {
        fun create(context: Context, provider: String): Context = if (Build.VERSION.SDK_INT >= 34)
            Api34ExplicitTtsProviderContext(context, provider) else ExplicitTtsProviderContext(context, provider)
    }
}

@TargetApi(34)
private class Api34ExplicitTtsProviderContext(context: Context, provider: String) : ExplicitTtsProviderContext(context, provider) {
    override fun bindService(service: Intent, flags: Context.BindServiceFlags, executor: Executor, connection: ServiceConnection): Boolean =
        allowed(service) && super.bindService(service, flags, executor, connection)
}
