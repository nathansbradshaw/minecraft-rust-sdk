package dev.minecraftsdk.mixin;

import com.mojang.authlib.GameProfile;
import dev.minecraftsdk.SdkService;
import net.minecraft.network.Connection;
import net.minecraft.network.protocol.login.ServerboundHelloPacket;
import net.minecraft.server.network.ServerLoginPacketListenerImpl;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** A local single-use admission ticket is required; ordinary Java authentication stays enabled. */
@Mixin(ServerLoginPacketListenerImpl.class)
public abstract class SdkLoginMixin {
    @Shadow @Final private Connection connection;
    @Shadow protected abstract void startClientVerification(GameProfile profile);
    @Inject(method="handleHello",at=@At(value="INVOKE",target="Lnet/minecraft/server/MinecraftServer;usesAuthentication()Z"),cancellable=true)
    private void sdk$admit(ServerboundHelloPacket packet,CallbackInfo callback) {
        var service=SdkService.active;
        if(service==null)return;
        service.admit(packet.profileId(),packet.name(),connection).ifPresent(profile->{startClientVerification(profile);callback.cancel();});
    }
}
