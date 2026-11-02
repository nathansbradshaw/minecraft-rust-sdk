package dev.minecraftsdk.mixin;

import dev.minecraftsdk.NativeChat;
import net.minecraft.network.PacketSendListener;
import net.minecraft.network.protocol.Packet;
import net.minecraft.server.network.ServerCommonPacketListenerImpl;
import net.minecraft.server.network.ServerGamePacketListenerImpl;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** Per-recipient outbound chat, including ordinary player, private and system chat. */
@Mixin(ServerCommonPacketListenerImpl.class)
public abstract class SdkDeliveredChatMixin {
    @Inject(method="send(Lnet/minecraft/network/protocol/Packet;Lnet/minecraft/network/PacketSendListener;)V",at=@At("RETURN"))
    private void sdk$delivered(Packet<?> packet,PacketSendListener listener,CallbackInfo callback){
        if((Object)this instanceof ServerGamePacketListenerImpl game)NativeChat.receive(game.player,packet);
    }
}
