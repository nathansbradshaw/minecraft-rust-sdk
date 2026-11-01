package dev.minecraftsdk.mixin;

import dev.minecraftsdk.SdkService;
import net.minecraft.network.Connection;
import net.minecraft.network.chat.*;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.network.*;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** Admit unsigned chat only for the exact private-ticket SDK connection; preserve normal Java secure chat. */
@Mixin(ServerGamePacketListenerImpl.class)
public abstract class SdkChatMixin {
    @Shadow private SignedMessageChain.Decoder signedMessageDecoder;
    @Inject(method="<init>",at=@At("RETURN"))
    private void sdk$chat(MinecraftServer server,Connection connection,ServerPlayer player,CommonListenerCookie cookie,CallbackInfo callback){
        SdkService service=SdkService.active;
        if(service==null||!service.owns(connection,player.getUUID()))return;
        var actual=player.getUUID();var original=signedMessageDecoder;
        java.util.function.BooleanSupplier admitted=()->SdkService.active==service&&connection.isConnected()&&actual.equals(player.getUUID())&&service.owns(connection,actual);
        var unsigned=SignedMessageChain.Decoder.unsigned(actual,()->!admitted.getAsBoolean());
        signedMessageDecoder=new SignedMessageChain.Decoder(){
            public PlayerChatMessage unpack(MessageSignature signature,SignedMessageBody body)throws SignedMessageChain.DecodeException{
                return signature==null?unsigned.unpack(null,body):original.unpack(signature,body);
            }
            public void setChainBroken(){original.setChainBroken();}
        };
    }
}
