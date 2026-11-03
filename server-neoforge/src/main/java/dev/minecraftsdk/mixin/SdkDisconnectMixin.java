package dev.minecraftsdk.mixin;

import dev.minecraftsdk.SdkService;
import dev.minecraftsdk.SessionMaterials;
import net.minecraft.network.DisconnectionDetails;
import net.minecraft.server.network.ServerGamePacketListenerImpl;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** Before ServerPlayer.disconnect and PlayerList.save, including broken sockets. */
@Mixin(ServerGamePacketListenerImpl.class)
public abstract class SdkDisconnectMixin {
    @Inject(method="onDisconnect",at=@At("HEAD"))
    private void sdk$preserveMaterials(DisconnectionDetails details,CallbackInfo callback){
        var service=SdkService.active;
        if(service!=null)SessionMaterials.close(service,((ServerGamePacketListenerImpl)(Object)this).player);
    }
}
